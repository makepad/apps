//! The trace's stages on the batch core: a batch of a part's layers through its coarse
//! grid (1 mm cells, layers as the grid's outer dimension), the cells that may hold the zero
//! set, compacted in id order, and per kept cell the signs of its 11 x 11 fine nodes (0.1 mm,
//! the grid unit) from as few field samples as the field's Lipschitz bound allows.
//!
//! The node rule (a Lipschitz cover): a value v at node n, with L the field's Lipschitz bound
//! per grid unit, proves |f| >= |v| - L * r at distance r from n, with v's sign. A node q takes
//! the bound b = |v| - L * |q - n| from a parent n, without a sample, when b >= 1.01 L: then q
//! and every point within one grid unit of q share the sign, so no fine edge at q changes sign,
//! every edge that does has two sampled ends, and a fine cell's centre (a saddle's test point)
//! is never past a covered corner's reach. A bound is itself a value for the next level. The
//! levels: the cell's corners (from the coarse pass), then the grids {0, 5, 10},
//! {0, 2, 3, 5, 7, 8, 10} and every node, each node's parents the corners of its cell in the
//! grid before; a level runs as one pass of cover checks (a thread per node) and one pass of
//! samples over the nodes it listed, so the samples gather along the zero set and run dense.
//!
//! Then the crossings: every owned fine edge whose ends change sign, listed in (cell, node row,
//! edge) order, each solved to a point and its outward unit gradient. Pass A settles a
//! crossing in four samples (a secant point, a forward-difference gradient there, a Newton
//! step inside the bracket and the sample that verifies it); pass B runs a sign search, eight
//! threads a crossing, on what pass A left (exact-zero ends, flat or grazing fields, kinks).
//!
//! Then the links: per crossing the crossing its span through the next fine cell ends at, so
//! that a layer's loops are the crossings in link order, the inside on their left.
use crate::batch::{Batch, CoreKernels, Size, NONE};
use crate::field::{spliced, PartProgram};
use crate::gpu::{u4, words_of, Gpu};
use makepad_platform::*;

script_mod! {
    use mod.pod.*
    use mod.math.*
    use mod.shader

    // The kernels that sample a part's field on its grid. grid: (nx, ny) coarse cells, the
    // batch's layers, the kept count word; frame rows 0..2: grid units (gx, gy) and z (mm) to
    // shape space, row 3: (the Lipschitz bound per grid unit, ...); zs: each layer's sample z.
    mod.gpuslice.Trace = {
        ..mod.sdf.Eval
        precise_math: true
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        grid: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        frame: shader.buffer(vec4f)
        zs: shader.buffer(f32)
        // The field at grid point (gx, gy) of layer p; an exact zero counts as inside.
        at: fn(gx: f32, gy: f32, p: u32) -> f32 {
            let z = self.zs[p]
            let a = self.frame[0u]
            let b = self.frame[1u]
            let c = self.frame[2u]
            let x = ((a.x * gx + a.y * gy) + a.z * z) + a.w
            let y = ((b.x * gx + b.y * gy) + b.z * z) + b.w
            let zz = ((c.x * gx + c.y * gy) + c.z * z) + c.w
            var d = self.eval_program(0.0, x, y, zz).x
            if d == 0.0 { d = 0.0 - asfloat(8388608u) }
            return d
        }
    }
    // One sample per coarse node (every 10 grid units) of every layer of the batch.
    mod.gpuslice.TraceCoarse = {
        ..mod.gpuslice.Trace
        coarse_d: shader.buffer_rw(f32)
        main: fn() {
            let e = global_id().x
            let nx1 = self.grid.x + 1u
            let nodes = nx1 * (self.grid.y + 1u)
            if e >= nodes * self.grid.z { return }
            let n = e % nodes
            self.coarse_d[e] = self.at(f32((n % nx1) * 10u), f32((n / nx1) * 10u), e / nodes)
        }
    }
    // A coarse cell is kept when its corners change sign or one is within the Lipschitz reach
    // of a zero (every point of the cell is within half its diagonal, sqrt(50) units, of a corner).
    mod.gpuslice.TraceKeep = {
        workgroup_size: 64
        grid: shader.uniform(vec4u)
        frame: shader.buffer(vec4f)
        coarse_d: shader.buffer(f32)
        flags: shader.buffer_rw(u32)
        main: fn() {
            let e = global_id().x
            let nx = self.grid.x
            let cells = nx * self.grid.y
            if e >= cells * self.grid.z { return }
            let c = e % cells
            let at = (e / cells) * (nx + 1u) * (self.grid.y + 1u) + (c / nx) * (nx + 1u) + c % nx
            let a = self.coarse_d[at]
            let b = self.coarse_d[at + 1u]
            let d = self.coarse_d[at + nx + 1u]
            let f = self.coarse_d[at + nx + 2u]
            let lo = min(min(a, b), min(d, f))
            let hi = max(max(a, b), max(d, f))
            let near = min(min(abs(a), abs(b)), min(abs(d), abs(f)))
            var keep = 0u
            if lo <= 0.0 && hi > 0.0 { keep = 1u }
            if near <= self.frame[3u].x * 7.08 { keep = 1u }
            self.flags[e] = keep
        }
    }
    // The kept cells in id order: kept[offs[e]] = e (layer * cells + cell), and the cells' slots
    // (coarse_slot[e]: its slot, NONE when not kept or past the cap). spec: (cells of the batch,
    // the slots' cap).
    mod.gpuslice.TraceKept = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        flags: shader.buffer(u32)
        offs: shader.buffer(u32)
        kept: shader.buffer_rw(u32)
        coarse_slot: shader.buffer_rw(u32)
        main: fn() {
            let e = global_id().x
            if e >= self.spec.x { return }
            var slot = 4294967295u
            if self.flags[e] != 0u {
                let o = self.offs[e]
                if o < self.spec.y {
                    self.kept[o] = e
                    slot = o
                }
            }
            self.coarse_slot[e] = slot
        }
    }
    // A kept cell's four corners into its nodes (node_d[slot * 121 + j * 11 + i]), from the
    // coarse pass: the first level of every cell.
    mod.gpuslice.TraceCorners = {
        workgroup_size: 64
        grid: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        kept: shader.buffer(u32)
        coarse_d: shader.buffer(f32)
        node_d: shader.buffer_rw(f32)
        main: fn() {
            let s = global_id().x
            if s >= self.counts[self.grid.w] { return }
            let e = self.kept[s]
            let nx = self.grid.x
            let cells = nx * self.grid.y
            let c = e % cells
            let nx1 = nx + 1u
            let at = (e / cells) * nx1 * (self.grid.y + 1u) + (c / nx) * nx1 + c % nx
            self.node_d[s * 121u] = self.coarse_d[at]
            self.node_d[s * 121u + 10u] = self.coarse_d[at + 1u]
            self.node_d[s * 121u + 110u] = self.coarse_d[at + nx1]
            self.node_d[s * 121u + 120u] = self.coarse_d[at + nx1 + 1u]
        }
    }
    // One level of the cover, a thread per (kept cell, node of the level): a node within reach
    // of one of its four parents (the corners of its cell in the level before) takes the best
    // parent's bound, sign * (|v| - L * distance), which is all a later level needs from it;
    // any other is listed for a sample. The list is compacted per workgroup and appended at a
    // base taken atomically: its order is the GPU's, but each sample lands at its node, so the
    // nodes do not depend on it. spec: x the level's first entry in `table` (pairs: node,
    // parents a byte each), y its entries, z the list's cap, w its count word.
    mod.gpuslice.TraceCover = {
        workgroup_size: 64
        spec: shader.uniform(vec4u)
        grid: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        frame: shader.buffer(vec4f)
        table: shader.buffer(u32)
        node_d: shader.buffer_rw(f32)
        list: shader.buffer_rw(u32)
        part: shader.workgroup(u32, 64)
        base: shader.workgroup(u32, 1)
        main: fn() {
            let g = global_id().x
            let li = local_index()
            let s = g / self.spec.y
            var need = 0u
            var node = 0u
            if s < self.counts[self.grid.w] {
                let k = self.spec.x + g % self.spec.y
                let m = self.table[2u * k]
                let parents = self.table[2u * k + 1u]
                let lu = self.frame[3u].x
                let qi = f32(m % 11u)
                let qj = f32(m / 11u)
                var bound = 0.0
                for t in 0..4u {
                    let c = (parents >> (t * 8u)) & 255u
                    let v = self.node_d[s * 121u + c]
                    let dx = f32(c % 11u) - qi
                    let dy = f32(c / 11u) - qj
                    let dist = sqrt(dx * dx + dy * dy)
                    let b = abs(v) - lu * dist
                    if b >= lu * 1.01 && b > abs(bound) {
                        bound = b
                        if v <= 0.0 { bound = 0.0 - b }
                    }
                }
                node = s * 121u + m
                if bound == 0.0 { need = 1u } else { self.node_d[node] = bound }
            }
            self.part[li] = need
            barrier()
            var d = 1u
            for r in 0..6u {
                var add = 0u
                if li >= d { add = self.part[li - d] }
                barrier()
                self.part[li] = self.part[li] + add
                barrier()
                d = d * 2u
            }
            if li == 63u {
                var at = 0u
                if self.part[63u] > 0u { at = atomic_add(self.counts[self.spec.w], self.part[63u]) }
                self.base[0u] = at
            }
            barrier()
            if need == 1u {
                let o = self.base[0u] + self.part[li] - 1u
                if o < self.spec.z { self.list[o] = node }
            }
        }
    }
    // The listed nodes' samples: list entry slot * 121 + node. spec.x: the list's count word.
    mod.gpuslice.TraceSample = {
        ..mod.gpuslice.Trace
        kept: shader.buffer(u32)
        list: shader.buffer(u32)
        node_d: shader.buffer_rw(f32)
        main: fn() {
            let g = global_id().x
            if g >= self.counts[self.spec.x] { return }
            let x = self.list[g]
            let s = x / 121u
            let m = x % 121u
            let e = self.kept[s]
            let nx = self.grid.x
            let cells = nx * self.grid.y
            let c = e % cells
            self.node_d[x] = self.at(f32((c % nx) * 10u + m % 11u), f32((c / nx) * 10u + m / 11u), e / cells)
        }
    }
    // A kept cell's sign-changing fine edges by node row, a thread per (cell, row j), eight
    // cells a workgroup: bits 0..9 the row's horizontal edges (node i to i + 1), bits 10..20 the
    // vertical edges up to row j + 1 (column i). An edge two cells share is owned by the lower
    // cell id: a bottom row edge by the cell below (its top row), a left column edge by the cell
    // to the left (its right column).
    mod.gpuslice.TraceEdge = {
        workgroup_size: 88
        spec: shader.uniform(vec4u)
        grid: shader.uniform(vec4u)
        counts: shader.buffer_rw(u32)
        kept: shader.buffer(u32)
        coarse_slot: shader.buffer(u32)
        node_d: shader.buffer(f32)
        rows: shader.workgroup(u32, 88)
        // Bit i set when node (i, j) of slot s is inside.
        row_signs: fn(s: u32, j: u32) -> u32 {
            var r = 0u
            for i in 0..11u {
                if self.node_d[s * 121u + j * 11u + i] <= 0.0 { r = r | (1u << i) }
            }
            return r
        }
        row_edges: fn(s: u32, j: u32) -> u32 {
            let r = self.row_signs(s, j)
            var h = (r ^ (r >> 1u)) & 1023u
            var v = 0u
            if j < 10u { v = (r ^ self.row_signs(s, j + 1u)) & 2047u }
            let e = self.kept[s]
            let nx = self.grid.x
            let c = e % (nx * self.grid.y)
            if j == 0u && c >= nx {
                if self.coarse_slot[e - nx] != 4294967295u { h = 0u }
            }
            if c % nx > 0u {
                if self.coarse_slot[e - 1u] != 4294967295u { v = v & 4294967294u }
            }
            return h | (v << 10u)
        }
        bit_count: fn(x: u32) -> u32 {
            var v = x - ((x >> 1u) & 1431655765u)
            v = (v & 858993459u) + ((v >> 2u) & 858993459u)
            v = (v + (v >> 4u)) & 252645135u
            return (v + (v >> 8u) + (v >> 16u) + (v >> 24u)) & 63u
        }
    }
    // Per kept cell its rows' edge bits (row_bits[slot * 11 + j]) and its owned crossings
    // (slot_cross).
    mod.gpuslice.TraceEdges = {
        ..mod.gpuslice.TraceEdge
        row_bits: shader.buffer_rw(u32)
        slot_cross: shader.buffer_rw(u32)
        main: fn() {
            let li = local_index()
            let s = group_id().x * 8u + li / 11u
            let j = li % 11u
            let live = s < self.counts[self.grid.w]
            var n = 0u
            if live {
                let bits = self.row_edges(s, j)
                self.row_bits[s * 11u + j] = bits
                n = self.bit_count(bits)
            }
            self.rows[li] = n
            barrier()
            if live && j == 0u {
                var t = 0u
                for r in 0..11u { t += self.rows[li + r] }
                self.slot_cross[s] = t
            }
        }
    }
    // The crossings' list in (cell, row, edge bit) order: crossing x = its cell's base (the scan
    // of slot_cross) + the crossings of the rows before (row_base) + its rank in the row;
    // cross_geo[x] = (gx, gy, axis | low end node << 1, slot). spec.x: the cap.
    mod.gpuslice.TraceCrossList = {
        ..mod.gpuslice.TraceEdge
        row_bits: shader.buffer(u32)
        cross_base: shader.buffer(u32)
        cross_geo: shader.buffer_rw(vec4u)
        row_base: shader.buffer_rw(u32)
        main: fn() {
            let li = local_index()
            let s = group_id().x * 8u + li / 11u
            let j = li % 11u
            let live = s < self.counts[self.grid.w]
            var bits = 0u
            if live { bits = self.row_bits[s * 11u + j] }
            self.rows[li] = self.bit_count(bits)
            barrier()
            if live {
                var o = self.cross_base[s]
                for r in 0..j { o += self.rows[li - j + r] }
                self.row_base[s * 11u + j] = o
                let e = self.kept[s]
                let nx = self.grid.x
                let c = e % (nx * self.grid.y)
                let gx = (c % nx) * 10u
                let gy = (c / nx) * 10u + j
                for b in 0..21u {
                    if ((bits >> b) & 1u) != 0u {
                        if o < self.spec.x {
                            var i = b
                            var axis = 0u
                            if b >= 10u { i = b - 10u; axis = 1u }
                            self.cross_geo[o] = vec4u(gx + i, gy, axis | ((j * 11u + i) << 1u), s)
                        }
                        o += 1u
                    }
                }
            }
        }
    }
    // A crossing's solve: its edge (cross_geo[x], from the list), its ends' values, and where it
    // lands: cross = (x, y, unit outward gradient) in grid units, cross_g = |gradient| per grid
    // unit, cross_key = the edge's key (2 * node + axis). spec: x the crossings' count word, y
    // the word that sums the samples the passes do not count by rule, z the retries' count word.
    mod.gpuslice.TraceSolve = {
        ..mod.gpuslice.Trace
        kept: shader.buffer(u32)
        node_d: shader.buffer(f32)
        cross_geo: shader.buffer(vec4u)
        cross: shader.buffer_rw(vec4f)
        cross_g: shader.buffer_rw(f32)
        cross_key: shader.buffer_rw(u32)
        edge_point: fn(gx: u32, gy: u32, axis: u32, t: f32) -> vec2 {
            if axis == 0u { return vec2(f32(gx) + t, f32(gy)) }
            return vec2(f32(gx), f32(gy) + t)
        }
        // The edge's (low, high) end values.
        ends: fn(g: vec4u) -> vec2 {
            let low = g.z >> 1u
            var high = low + 1u
            if (g.z & 1u) == 1u { high = low + 11u }
            return vec2(self.node_d[g.w * 121u + low], self.node_d[g.w * 121u + high])
        }
        // The regula falsi point of a bracket (lo, hi, f(lo), f(hi)), kept off its ends.
        secant: fn(br: vec4) -> f32 {
            let w = br.y - br.x
            let t = br.x + w * (br.z / (br.z - br.w))
            return clamp(t, br.x + 0.001 * w, br.y - 0.001 * w)
        }
        put: fn(x: u32, g: vec4u, q: vec2, n: vec2, gl: f32) {
            self.cross[x] = vec4(q.x, q.y, n.x, n.y)
            self.cross_g[x] = gl
            self.cross_key[x] = 2u * (g.y * (self.grid.x * 10u + 1u) + g.x) + (g.z & 1u)
        }
    }
    // Pass A, every crossing in one thread, four samples: the secant point t1 of the edge's ends,
    // the gradient there by forward differences 0.02 units along x and y, a Newton step along
    // the edge to t2 (inside the bracket t1 leaves), and a sample at t2 to verify it: accepted
    // when the step left from t2 is at most 5e-4 units (5e-5 mm), and the crossing is t2 less
    // that step. With the gradient clear of flat, an exact zero at t1 or t2 is the crossing (the
    // f32 field rounds to zero within ~1e-6 mm of it), so only an exact-zero end needs pass B,
    // where the inside's start decides. That, a flat or grazing gradient, or a step that
    // leaves the bracket or does not settle (a kink) goes to pass B by an atomic append (its
    // samples summed there; an accepted crossing takes four).
    mod.gpuslice.TraceCross = {
        ..mod.gpuslice.TraceSolve
        retry: shader.buffer_rw(u32)
        main: fn() {
            let x = global_id().x
            if x >= self.counts[self.spec.x] { return }
            let g = self.cross_geo[x]
            let axis = g.z & 1u
            let ab = self.ends(g)
            let a = ab.x
            let p = self.kept[g.w] / (self.grid.x * self.grid.y)
            let zero = 0.0 - asfloat(8388608u)
            var taken = 0u
            var done = 0u
            if a != zero && ab.y != zero {
                let t1 = self.secant(vec4(0.0, 1.0, a, ab.y))
                let q = self.edge_point(g.x, g.y, axis, t1)
                let v1 = self.at(q.x, q.y, p)
                let fx = self.at(q.x + 0.02, q.y, p)
                let fy = self.at(q.x, q.y + 0.02, p)
                taken = 3u
                // the offsets the samples actually took, exactly (Sterbenz)
                let grad = vec2((fx - v1) / ((q.x + 0.02) - q.x), (fy - v1) / ((q.y + 0.02) - q.y))
                let gl = length(grad)
                var ga = grad.x
                if axis == 1u { ga = grad.y }
                if abs(ga) >= 0.1 * gl && gl >= 0.01 * self.frame[3u].x {
                    let t2 = t1 - v1 / ga
                    var lo = 0.0
                    var hi = t1
                    if (v1 <= 0.0) == (a <= 0.0) { lo = t1; hi = 1.0 }
                    if t2 >= lo && t2 <= hi {
                        let q2 = self.edge_point(g.x, g.y, axis, t2)
                        let v2 = self.at(q2.x, q2.y, p)
                        taken = 4u
                        let step = v2 / ga
                        if abs(step) <= 0.0005 {
                            self.put(x, g, self.edge_point(g.x, g.y, axis, t2 - step), grad / gl, gl)
                            done = 1u
                        }
                    }
                }
            }
            if done == 0u {
                let was = atomic_add(self.counts[self.spec.y], taken)
                let o = atomic_add(self.counts[self.spec.z], 1u)
                self.retry[o] = x
            }
        }
    }
    // Pass B, the crossings pass A left (retry[0..counts[spec.z]]), eight threads each (eight
    // crossings a workgroup): a sign search over the edge's bracket, four rounds of eight samples
    // that cut it into nine and keep the first part whose far end leaves the low end's side
    // (inside: f <= 0, so an exact zero counts as inside and a face flat at zero ends where the
    // inside starts), 9^-4 units at the end, then four central-difference samples 0.01 units
    // round its middle, a thread each. No step trusts the field's shape, so a kink, a field flat
    // past the crossing or an exact-zero end take the same four rounds.
    mod.gpuslice.TraceCrossRetry = {
        ..mod.gpuslice.TraceSolve
        retry: shader.buffer(u32)
        signs: shader.workgroup(u32, 64)
        near: shader.workgroup(f32, 64)
        main: fn() {
            let li = local_index()
            let k = li % 8u
            let r = group_id().x * 8u + li / 8u
            let live = r < self.counts[self.spec.z]
            var x = 0u
            var g = vec4u(0u)
            var a = 0.0
            var p = 0u
            if live {
                x = self.retry[r]
                g = self.cross_geo[x]
                a = self.ends(g).x
                p = self.kept[g.w] / (self.grid.x * self.grid.y)
            }
            let axis = g.z & 1u
            var side = 0u
            if a <= 0.0 { side = 1u }
            var lo = 0.0
            var w = 1.0
            for round in 0..4u {
                var inside = side
                if live {
                    let q = self.edge_point(g.x, g.y, axis, lo + f32(k + 1u) * w / 9.0)
                    if self.at(q.x, q.y, p) <= 0.0 { inside = 1u } else { inside = 0u }
                }
                self.signs[li] = inside
                barrier()
                var first = 8u
                for j in 0..8u {
                    if first == 8u && self.signs[li - k + j] != side { first = j }
                }
                barrier()
                lo = lo + f32(first) * w / 9.0
                w = w / 9.0
            }
            let q = self.edge_point(g.x, g.y, axis, lo + 0.5 * w)
            var d = 0.0
            if live && k < 4u {
                var o = vec2(0.01, 0.0)
                if k >= 2u { o = vec2(0.0, 0.01) }
                if k % 2u == 1u { o = vec2(0.0) - o }
                d = self.at(q.x + o.x, q.y + o.y, p)
            }
            self.near[li] = d
            barrier()
            if live && k == 0u {
                let b = li - k
                // the offsets the samples actually took, exactly (Sterbenz)
                let dx = (q.x + 0.01 - q.x) + (q.x - (q.x - 0.01))
                let dy = (q.y + 0.01 - q.y) + (q.y - (q.y - 0.01))
                let grad = vec2((self.near[b] - self.near[b + 1u]) / dx, (self.near[b + 2u] - self.near[b + 3u]) / dy)
                let gl = length(grad)
                var n = vec2(0.0)
                if gl > 0.000000000000000001 { n = grad / gl }
                else {
                    // a flat field here: outward along the edge, from its inside end
                    var s = 1.0
                    if side == 0u { s = 0.0 - 1.0 }
                    if axis == 0u { n = vec2(s, 0.0) } else { n = vec2(0.0, s) }
                }
                self.put(x, g, q, n, gl)
                let was = atomic_add(self.counts[self.spec.y], 36u)
            }
        }
    }
    // The loops' links: per crossing c, the crossing its span ends at (next[c]); a loop is the
    // crossings in next order, the inside on its left (outer boundaries counterclockwise). The
    // span runs through the fine cell where a counterclockwise walk of the cell's boundary
    // (bottom, right, top, left) leaves the inside at c, and ends where the walk re-enters it. A
    // saddle (four crossings) pairs a start with the next end when its centre is inside, else
    // with the previous one; the centre takes one sample. A crossing's index from its edge: the
    // edge's owner cell's row base (row_base, from the list) plus its rank among the row's bits.
    // spec.x: the crossings' count word; next[c] = NONE when an end is missing (a broken loop the
    // host check finds).
    mod.gpuslice.TraceLinks = {
        ..mod.gpuslice.Trace
        kept: shader.buffer(u32)
        coarse_slot: shader.buffer(u32)
        node_d: shader.buffer(f32)
        cross_geo: shader.buffer(vec4u)
        row_bits: shader.buffer(u32)
        row_base: shader.buffer(u32)
        next: shader.buffer_rw(u32)
        bit_count: fn(x: u32) -> u32 {
            var v = x - ((x >> 1u) & 1431655765u)
            v = (v & 858993459u) + ((v >> 2u) & 858993459u)
            v = (v + (v >> 4u)) & 252645135u
            return (v + (v >> 8u) + (v >> 16u) + (v >> 24u)) & 63u
        }
        // The slot of coarse cell (cx, cy) of layer p, NONE when it is not kept.
        slot_at: fn(cx: u32, cy: u32, p: u32) -> u32 {
            return self.coarse_slot[(p * self.grid.y + cy) * self.grid.x + cx]
        }
        // The crossing on fine edge (gx, gy, axis) of layer p: its owner cell (the lower cell
        // id of the two that share it), that cell's local row and bit, then the row's base plus
        // the bits below.
        cross_at: fn(gx: u32, gy: u32, axis: u32, p: u32) -> u32 {
            var cx = min(gx / 10u, self.grid.x - 1u)
            var cy = min(gy / 10u, self.grid.y - 1u)
            var i = gx - cx * 10u
            var j = gy - cy * 10u
            if axis == 0u && j == 0u && cy > 0u {
                if self.slot_at(cx, cy - 1u, p) != 4294967295u { cy = cy - 1u; j = 10u }
            }
            if axis == 1u && i == 0u && cx > 0u {
                if self.slot_at(cx - 1u, cy, p) != 4294967295u { cx = cx - 1u; i = 10u }
            }
            let s = self.slot_at(cx, cy, p)
            if s == 4294967295u { return s }
            var b = i
            if axis == 1u { b = 10u + i }
            let bits = self.row_bits[s * 11u + j]
            if ((bits >> b) & 1u) == 0u { return 4294967295u }
            return self.row_base[s * 11u + j] + self.bit_count(bits & ((1u << b) - 1u))
        }
        main: fn() {
            let c = global_id().x
            if c >= self.counts[self.spec.x] { return }
            let g = self.cross_geo[c]
            let axis = g.z & 1u
            let p = self.kept[g.w] / (self.grid.x * self.grid.y)
            let low_in = self.node_d[g.w * 121u + (g.z >> 1u)] <= 0.0
            // the span's fine cell (fx, fy) and c's edge in its walk: 0 bottom, 1 right, 2 top, 3 left
            var fx = g.x
            var fy = g.y
            var k = 0u
            if axis == 0u {
                if !low_in { fy = g.y - 1u; k = 2u }
            } else {
                if low_in { fx = g.x - 1u; k = 1u } else { k = 3u }
            }
            let s = self.slot_at(fx / 10u, fy / 10u, p)
            var end = 4294967295u
            if s != 4294967295u {
                let m = (fy % 10u) * 11u + fx % 10u
                // inside bits of the corners in walk order: bottom left, bottom right, top right, top left
                var ins = 0u
                if self.node_d[s * 121u + m] <= 0.0 { ins = ins | 1u }
                if self.node_d[s * 121u + m + 1u] <= 0.0 { ins = ins | 2u }
                if self.node_d[s * 121u + m + 12u] <= 0.0 { ins = ins | 4u }
                if self.node_d[s * 121u + m + 11u] <= 0.0 { ins = ins | 8u }
                // edge e changes sign when corners e and e + 1 differ
                let flips = (ins ^ ((ins >> 1u) | ((ins & 1u) << 3u))) & 15u
                var e = (k + 1u) % 4u
                if flips == 15u {
                    // a saddle: the next end (k + 1) when the centre is inside, else the previous (k + 3)
                    if self.at(f32(fx) + 0.5, f32(fy) + 0.5, p) > 0.0 { e = (k + 3u) % 4u }
                } else {
                    for t in 0..3u {
                        if ((flips >> e) & 1u) == 0u { e = (e + 1u) % 4u }
                    }
                }
                var ex = fx
                var ey = fy
                var ea = 0u
                if e == 1u { ex = fx + 1u; ea = 1u }
                if e == 2u { ey = fy + 1u }
                if e == 3u { ea = 1u }
                end = self.cross_at(ex, ey, ea, p)
            }
            self.next[c] = end
        }
    }
    // The reference: every fine node of every kept cell sampled (dense_d, as node_d), one
    // thread a node.
    mod.gpuslice.TraceDense = {
        ..mod.gpuslice.Trace
        kept: shader.buffer(u32)
        dense_d: shader.buffer_rw(f32)
        main: fn() {
            let g = global_id().x
            let s = g / 121u
            if s >= self.counts[self.grid.w] { return }
            let m = g % 121u
            let e = self.kept[s]
            let nx = self.grid.x
            let cells = nx * self.grid.y
            let c = e % cells
            self.dense_d[g] = self.at(f32((c % nx) * 10u + m % 11u), f32((c / nx) * 10u + m / 11u), e / cells)
        }
    }
}

/// The 1 mm lattice's phase: grid nodes sit off round coordinates, so an axis-aligned face is
/// never exactly on a node row.
pub const PHASE: f64 = 0.0371;
/// Grid units a coarse cell.
pub const CELL: u32 = 10;
/// Fine nodes a kept cell.
pub const NODES: u32 = 121;

/// A part revision's grid: origin in printer mm (the 1 mm lattice less [`PHASE`]) and its
/// coarse cells, with a cell of margin round the part so no zero touches the border.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub origin: [f64; 2],
    pub nx: u32,
    pub ny: u32,
}

impl Grid {
    pub fn around(lo: [f64; 2], hi: [f64; 2]) -> Grid {
        let origin = [(lo[0] - 1.0).floor() - PHASE, (lo[1] - 1.0).floor() - PHASE];
        let nx = ((hi[0] + 1.0 - origin[0]).ceil() as u32).max(1);
        let ny = ((hi[1] + 1.0 - origin[1]).ceil() as u32).max(1);
        Grid { origin, nx, ny }
    }
    pub fn cells(&self) -> u32 { self.nx * self.ny }
    pub fn nodes(&self) -> u32 { (self.nx + 1) * (self.ny + 1) }
    /// Printer mm of grid point (gx, gy).
    pub fn mm(&self, gx: f64, gy: f64) -> [f64; 2] { [self.origin[0] + gx * 0.1, self.origin[1] + gy * 0.1] }
}

/// The `frame` rows: grid units and z (mm) to shape space, composed in f64; row 3 the
/// Lipschitz bound per grid unit.
pub fn frame_rows(to_shape: [[f64; 4]; 3], grid: &Grid, lipschitz: f64) -> [[f32; 4]; 4] {
    let mut rows = [[0.0f32; 4]; 4];
    for (r, m) in to_shape.iter().enumerate() {
        rows[r] = [(m[0] * 0.1) as f32, (m[1] * 0.1) as f32, m[2] as f32, (m[0] * grid.origin[0] + m[1] * grid.origin[1] + m[3]) as f32];
    }
    rows[3] = [(lipschitz * 0.1) as f32, 0.0, 0.0, 0.0];
    rows
}

/// The grids of the cover's levels, coarse to fine (level 0: the corners).
const LEVELS: [&[u32]; 4] = [&[0, 10], &[0, 5, 10], &[0, 2, 3, 5, 7, 8, 10], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]];

/// The cover's table: per level after the corners, its nodes (j * 11 + i) each with its four
/// parents (a byte each), and per level (first entry, entries).
pub fn level_table() -> (Vec<u32>, Vec<(u32, u32)>) {
    let (mut table, mut levels) = (Vec::new(), Vec::new());
    for w in LEVELS.windows(2) {
        let (prev, grid) = (w[0], w[1]);
        let first = table.len() as u32 / 2;
        let below = |x: u32| *prev.iter().rev().find(|&&g| g <= x).unwrap();
        let above = |x: u32| *prev.iter().find(|&&g| g >= x).unwrap();
        for &j in grid {
            for &i in grid {
                if prev.contains(&i) && prev.contains(&j) { continue; }
                let parents = [(below(i), below(j)), (above(i), below(j)), (below(i), above(j)), (above(i), above(j))];
                table.push(j * 11 + i);
                table.push(parents.iter().enumerate().fold(0, |p, (t, &(a, b))| p | (b * 11 + a) << (8 * t)));
            }
        }
        levels.push((first, table.len() as u32 / 2 - first));
    }
    (table, levels)
}

/// The trace kernels with one part's field spliced in.
pub struct TraceKernels {
    pub core: CoreKernels,
    coarse: ComputeKernel,
    keep: ComputeKernel,
    kept: ComputeKernel,
    corners: ComputeKernel,
    cover: ComputeKernel,
    sample: ComputeKernel,
    edges: ComputeKernel,
    cross_list: ComputeKernel,
    cross: ComputeKernel,
    retry: ComputeKernel,
    links: ComputeKernel,
    dense: ComputeKernel,
}

impl TraceKernels {
    pub fn new(gpu: &mut Gpu, part: &PartProgram) -> Result<TraceKernels, String> {
        let plain = |gpu: &mut Gpu, name| gpu.kernel(id!(gpuslice), name);
        Ok(TraceKernels {
            core: CoreKernels::new(gpu)?,
            coarse: spliced(gpu, &part.slots, id!(TraceCoarse))?,
            keep: plain(gpu, id!(TraceKeep))?,
            kept: plain(gpu, id!(TraceKept))?,
            corners: plain(gpu, id!(TraceCorners))?,
            cover: plain(gpu, id!(TraceCover))?,
            sample: spliced(gpu, &part.slots, id!(TraceSample))?,
            edges: plain(gpu, id!(TraceEdges))?,
            cross_list: plain(gpu, id!(TraceCrossList))?,
            cross: spliced(gpu, &part.slots, id!(TraceCross))?,
            retry: spliced(gpu, &part.slots, id!(TraceCrossRetry))?,
            links: spliced(gpu, &part.slots, id!(TraceLinks))?,
            dense: spliced(gpu, &part.slots, id!(TraceDense))?,
        })
    }
}

/// A part on the GPU for tracing: its grid, frame, program and every layer's sample z.
pub struct PartTrace {
    pub grid: Grid,
    pub zs: Vec<f64>,
    frame: GpuBuffer,
    program: GpuBuffer,
    table: GpuBuffer,
    levels: Vec<(u32, u32)>,
}

impl PartTrace {
    /// `zs`: each layer's sample z (printer mm); `bounds`: the part's box (printer mm).
    pub fn new(gpu: &mut Gpu, part: &PartProgram, lipschitz: f64, bounds: ([f64; 3], [f64; 3]), zs: Vec<f64>) -> Result<PartTrace, String> {
        let grid = Grid::around([bounds.0[0], bounds.0[1]], [bounds.1[0], bounds.1[1]]);
        let rows = frame_rows(part.to_shape, &grid, lipschitz);
        let frame = gpu.words(&rows.iter().flatten().map(|f| f.to_bits()).collect::<Vec<u32>>(), false)?;
        let program = gpu.words(&part.words(), false)?;
        let (table, levels) = level_table();
        let table = gpu.words(&table, false)?;
        Ok(PartTrace { grid, zs, frame, program, table, levels })
    }
}

/// A batch's room: kept cells and crossings. A batch past either delivers nothing and runs
/// again with that room doubled.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub slots: u32,
    pub crosses: u32,
}

/// How a batch runs: GPU timers per dispatch, the dense node reference, readbacks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Opts {
    pub timed: bool,
    pub dense: bool,
    pub read: bool,
}

/// A crossing read back: (x, y) in grid units, the unit outward gradient, |gradient| per grid
/// unit, its edge key and its batch layer.
#[derive(Clone, Copy, Debug)]
pub struct Crossing {
    pub at: [f32; 2],
    pub normal: [f32; 2],
    pub gradient: f32,
    pub key: u32,
    pub layer: u32,
}

/// What a batch of layers gave: its counts, and (when read back) the kept cells' ids, their
/// nodes (with the dense reference when asked for) and the crossings.
pub struct TraceRun {
    /// Which room ran out (the batch delivered nothing): kept cells, crossings.
    pub overflow: Option<(bool, bool)>,
    pub kept: u32,
    pub node_samples: u32,
    pub crosses: u32,
    pub cross_samples: u32,
    /// The crossings pass A left to pass B, and (when read back) which.
    pub retries: u32,
    pub retried: Vec<u32>,
    /// Per crossing the crossing its span ends at (NONE: a broken loop).
    pub next: Vec<u32>,
    pub kept_ids: Vec<u32>,
    pub node_d: Vec<f32>,
    pub dense_d: Vec<f32>,
    pub crossings: Vec<Crossing>,
    /// GPU ms per (stage, dispatch label) (timed runs), wall ms from submit to the last read.
    pub label_ms: Vec<(String, String, f64)>,
    pub wall_ms: f64,
}

/// Counts words of a trace batch: the kept cells, the crossings, the samples the solve passes
/// sum (pass B's, and pass A's for the crossings it left), pass B's crossings, then each cover
/// level's listed samples.
const W_KEPT: u32 = 1;
const W_CROSS: u32 = 2;
const W_CROSS_SAMPLES: u32 = 3;
const W_RETRY: u32 = 4;
const W_LEVEL: u32 = 5;
/// Samples pass A takes for a crossing it settles.
const PASS_A_SAMPLES: u32 = 4;

/// Layers `layers` of `part` through the trace's stages in one submission.
pub fn trace_batch(gpu: &mut Gpu, k: &TraceKernels, part: &PartTrace, layers: std::ops::Range<usize>, caps: Caps, opts: Opts) -> Result<TraceRun, String> {
    let g = part.grid;
    let n = layers.len() as u32;
    let first_layer = layers.start as u32;
    let cells = g.cells() * n;
    let nodes = g.nodes() * n;
    let (cap, xcap) = (caps.slots, caps.crosses);
    let zs: Vec<u32> = part.zs[layers].iter().map(|&z| (z as f32).to_bits()).collect();
    let zs = gpu.words(&zs, false)?;
    let coarse_d = gpu.buffer(nodes as u64 * 4, false)?;
    let flags = gpu.buffer(cells as u64 * 4, false)?;
    let offs = gpu.buffer(cells as u64 * 4, false)?;
    let sums = gpu.buffer(cells.div_ceil(256) as u64 * 4, false)?;
    let coarse_slot = gpu.buffer(cells as u64 * 4, false)?;
    let kept = gpu.buffer(cap as u64 * 4, false)?;
    let node_d = gpu.buffer(cap as u64 * NODES as u64 * 4, false)?;
    let dense_d = gpu.buffer(if opts.dense { cap as u64 * NODES as u64 * 4 } else { 4 }, false)?;
    let levels = part.levels.len() as u32;
    let list_cap = cap * part.levels.iter().map(|l| l.1).max().unwrap_or(1);
    let list = gpu.buffer(list_cap as u64 * 4, false)?;
    let row_bits = gpu.buffer(cap as u64 * 11 * 4, false)?;
    let row_base = gpu.buffer(cap as u64 * 11 * 4, false)?;
    let slot_cross = gpu.buffer(cap as u64 * 4, false)?;
    let cross_base = gpu.buffer(cap as u64 * 4, false)?;
    let slot_sums = gpu.buffer(cap.div_ceil(256) as u64 * 4, false)?;
    let cross_geo = gpu.buffer(xcap as u64 * 16, false)?;
    let cross = gpu.buffer(xcap as u64 * 16, false)?;
    let cross_g = gpu.buffer(xcap as u64 * 4, false)?;
    let cross_key = gpu.buffer(xcap as u64 * 4, false)?;
    let retry = gpu.buffer(xcap as u64 * 4, false)?;
    let next = gpu.buffer(xcap as u64 * 4, false)?;

    // indirect slots: 0 cells, 1 dense, 2 crossings, 3 cells by eight (88 rows a workgroup),
    // 4 retried crossings by eight (64 threads a workgroup), then two a cover level
    let mut b = Batch::new(gpu, "trace", W_LEVEL + levels, 5 + 2 * levels, opts.timed)?;
    let gridu = u4([g.nx, g.ny, n, W_KEPT]);
    // a field kernel's bindings: the part's program, frame and layers, then its own
    let field = [(id!(program), &part.program), (id!(frame), &part.frame), (id!(zs), &zs)];
    let with = |extra: &[(LiveId, &GpuBuffer)]| -> Vec<(LiveId, GpuBuffer)> { field.iter().chain(extra).map(|(n, x)| (*n, (*x).clone())).collect() };
    macro_rules! bufs { ($v:expr) => { &$v.iter().map(|(n, x)| (*n, x)).collect::<Vec<(LiveId, &GpuBuffer)>>() } }

    b.stage("coarse");
    b.run_with("coarse", &k.coarse, Size::Direct([nodes.div_ceil(64), 1, 1]), &[(id!(grid), &gridu)], bufs!(with(&[(id!(coarse_d), &coarse_d)])))?;
    b.stage("keep");
    b.run_with("keep", &k.keep, Size::Direct([cells.div_ceil(64), 1, 1]), &[(id!(grid), &gridu)],
        &[(id!(frame), &part.frame), (id!(coarse_d), &coarse_d), (id!(flags), &flags)])?;
    b.scan(&k.core, &flags, &offs, &sums, cells, NONE, W_KEPT)?;
    b.run("kept", &k.kept, Size::Direct([cells.div_ceil(64), 1, 1]), [cells, cap, 0, 0],
        &[(id!(flags), &flags), (id!(offs), &offs), (id!(kept), &kept), (id!(coarse_slot), &coarse_slot)])?;

    b.stage("nodes");
    let per_cell = b.counted(&k.core, 0, W_KEPT, cap, 1)?;
    b.run_with("corners", &k.corners, per_cell, &[(id!(grid), &gridu)], &[(id!(kept), &kept), (id!(coarse_d), &coarse_d), (id!(node_d), &node_d)])?;
    for (l, &(first, entries)) in part.levels.iter().enumerate() {
        let (word, slot) = (W_LEVEL + l as u32, 5 + 2 * l as u32);
        let size = b.counted(&k.core, slot, W_KEPT, cap, entries)?;
        b.run_with("cover", &k.cover, size, &[(id!(spec), &u4([first, entries, list_cap, word])), (id!(grid), &gridu)],
            &[(id!(frame), &part.frame), (id!(table), &part.table), (id!(node_d), &node_d), (id!(list), &list)])?;
        let size = b.counted(&k.core, slot + 1, word, list_cap, 1)?;
        b.run_with("sample", &k.sample, size, &[(id!(spec), &u4([word, 0, 0, 0])), (id!(grid), &gridu)],
            bufs!(with(&[(id!(kept), &kept), (id!(list), &list), (id!(node_d), &node_d)])))?;
    }

    b.stage("crossings");
    let edge_bufs = [(id!(kept), &kept), (id!(coarse_slot), &coarse_slot), (id!(node_d), &node_d)];
    let by_eight = b.counted(&k.core, 3, W_KEPT, cap, 8)?;
    let mut v: Vec<(LiveId, &GpuBuffer)> = edge_bufs.to_vec();
    v.extend([(id!(row_bits), &row_bits), (id!(slot_cross), &slot_cross)]);
    b.run_with("edges", &k.edges, by_eight, &[(id!(grid), &gridu)], &v)?;
    b.scan(&k.core, &slot_cross, &cross_base, &slot_sums, cap, W_KEPT, W_CROSS)?;
    let mut v: Vec<(LiveId, &GpuBuffer)> = edge_bufs.to_vec();
    v.extend([(id!(row_bits), &row_bits), (id!(cross_base), &cross_base), (id!(cross_geo), &cross_geo), (id!(row_base), &row_base)]);
    b.run_with("list", &k.cross_list, by_eight, &[(id!(spec), &u4([xcap, 0, 0, 0])), (id!(grid), &gridu)], &v)?;
    let per_cross = b.counted(&k.core, 2, W_CROSS, xcap, 1)?;
    let solve = u4([W_CROSS, W_CROSS_SAMPLES, W_RETRY, 0]);
    let solved = [(id!(kept), &kept), (id!(node_d), &node_d), (id!(cross_geo), &cross_geo), (id!(cross), &cross), (id!(cross_g), &cross_g), (id!(cross_key), &cross_key), (id!(retry), &retry)];
    b.run_with("solve", &k.cross, per_cross, &[(id!(spec), &solve), (id!(grid), &gridu)], bufs!(with(&solved)))?;
    let per_retry = b.counted(&k.core, 4, W_RETRY, xcap, 8)?;
    b.run_with("retry", &k.retry, per_retry, &[(id!(spec), &solve), (id!(grid), &gridu)], bufs!(with(&solved)))?;

    b.stage("links");
    let linked = [(id!(kept), &kept), (id!(coarse_slot), &coarse_slot), (id!(node_d), &node_d), (id!(cross_geo), &cross_geo), (id!(row_bits), &row_bits), (id!(row_base), &row_base), (id!(next), &next)];
    b.run_with("links", &k.links, per_cross, &[(id!(spec), &u4([W_CROSS, 0, 0, 0])), (id!(grid), &gridu)], bufs!(with(&linked)))?;

    if opts.dense {
        b.stage("dense");
        let size = b.counted(&k.core, 1, W_KEPT, cap, NODES)?;
        b.run_with("dense", &k.dense, size, &[(id!(grid), &gridu)], bufs!(with(&[(id!(kept), &kept), (id!(dense_d), &dense_d)])))?;
    }
    let mut reads: Vec<(&GpuBuffer, u64, u64)> = Vec::new();
    if opts.read {
        reads.extend([(&kept, 0, cap as u64 * 4), (&node_d, 0, cap as u64 * NODES as u64 * 4)]);
        reads.extend([(&cross_geo, 0, xcap as u64 * 16), (&cross, 0, xcap as u64 * 16), (&cross_g, 0, xcap as u64 * 4), (&cross_key, 0, xcap as u64 * 4), (&retry, 0, xcap as u64 * 4), (&next, 0, xcap as u64 * 4)]);
        if opts.dense { reads.push((&dense_d, 0, cap as u64 * NODES as u64 * 4)); }
    }
    let ran = b.submit(gpu, &reads)?;
    let label_ms = if opts.timed { ran.label_ms(gpu) } else { Vec::new() };
    let c = &ran.counts;
    let overflow = (c[0] != 0).then(|| (c[W_KEPT as usize] >= cap, c[W_CROSS as usize] >= xcap));
    let (kept_n, cross_n) = (c[W_KEPT as usize].min(cap), c[W_CROSS as usize].min(xcap));
    let node_samples = (0..levels).map(|l| c[(W_LEVEL + l) as usize]).sum();
    let retries = c[W_RETRY as usize];
    let floats = |bytes: &[u8], n: usize| words_of(&bytes[..n * 4]).into_iter().map(f32::from_bits).collect::<Vec<f32>>();
    let cross_samples = c[W_CROSS_SAMPLES as usize] + PASS_A_SAMPLES * cross_n.saturating_sub(retries);
    let mut run = TraceRun { overflow, kept: kept_n, node_samples, crosses: cross_n, cross_samples, retries, retried: Vec::new(), next: Vec::new(),
        kept_ids: Vec::new(), node_d: Vec::new(), dense_d: Vec::new(), crossings: Vec::new(), label_ms, wall_ms: ran.wall_ms };
    if opts.read && overflow.is_none() {
        let r = &ran.reads;
        let nn = kept_n as usize * NODES as usize;
        run.kept_ids = words_of(&r[0][..kept_n as usize * 4]);
        run.node_d = floats(&r[1], nn);
        let (geo, at, gl, key) = (words_of(&r[2]), floats(&r[3], cross_n as usize * 4), floats(&r[4], cross_n as usize), words_of(&r[5]));
        run.crossings = (0..cross_n as usize).map(|x| Crossing {
            at: [at[4 * x], at[4 * x + 1]],
            normal: [at[4 * x + 2], at[4 * x + 3]],
            gradient: gl[x],
            key: key[x],
            layer: first_layer + run.kept_ids[geo[4 * x + 3] as usize] / g.cells(),
        }).collect();
        run.retried = words_of(&r[6][..retries as usize * 4]);
        run.next = words_of(&r[7][..cross_n as usize * 4]);
        if opts.dense { run.dense_d = floats(&r[8], nn); }
    }
    Ok(run)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every node is in one level (the corners in the first); a node's parents are the corners
    /// of its cell in the grid before.
    #[test]
    fn levels_cover_every_node_once() {
        let (table, levels) = level_table();
        let mut seen = [false; 121];
        for m in [0, 10, 110, 120] { seen[m] = true; }
        for (l, &(first, entries)) in levels.iter().enumerate() {
            let prev = LEVELS[l];
            for k in first..first + entries {
                let (m, parents) = (table[2 * k as usize], table[2 * k as usize + 1]);
                assert!(!seen[m as usize], "node {m} twice");
                seen[m as usize] = true;
                for t in 0..4 {
                    let c = (parents >> (8 * t)) & 255;
                    let (ci, cj, i, j) = (c % 11, c / 11, m % 11, m / 11);
                    assert!(prev.contains(&ci) && prev.contains(&cj), "parent {c} of {m} not in the grid before");
                    assert!(ci.abs_diff(i) <= 5 && cj.abs_diff(j) <= 5);
                }
            }
        }
        assert!(seen.iter().all(|&s| s));
        assert_eq!(levels.iter().map(|l| l.1).collect::<Vec<_>>(), vec![5, 40, 72]);
    }
}

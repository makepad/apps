//! `m.veins`: a slime mould's veins raised on a solid's skin as tubes. The host grows the mould
//! (`Physarum{..}`) on the source, finds the middle of each vein and its half-width along it and
//! raises a round cone on each straight piece of it, keeping those that print without support; it
//! passes them back in [`super::Options::solved`] under the part, twelve words a tube in the
//! grid's table (a, ra, b, rb, the cut's normal and its offset: see [`super::VeinsRequest`]). The
//! lowering lists them by buckets of space for the library's `veins` kernel, which reads the
//! least distance to those near. Until the host answers there are none. Its box is its tubes'
//! (each round cone on its cut's side), so a part they stand on keeps its own bottom.

use super::{csg_sdf, Diag, Fields, Lower, PatternArg, Res, P3};
use csg_sdf::SdfKernel;
use makepad_script::ScriptValue;

/// The words a tube takes in the host's answer and in the lists.
pub const TUBE_WORDS: usize = 12;
/// How far from the tubes the field reads its true distance (metres): past it the field holds
/// at this, so a list holds every tube within it of its bucket.
const REACH: f64 = 0.002;
/// A bucket's side (metres), and the most buckets a table has (a coarser grid past it).
const BUCKET: f64 = 0.002;
const MAX_BUCKETS: f64 = 4_000_000.0;
/// The words a table's index reads exactly (an f32 holds a whole number to this).
const EXACT_WORD: usize = 1 << 24;

impl Lower<'_, '_, '_> {
    /// `m.veins(source, Physarum{..})`: asks the host for the tubes and lists those it passed in.
    pub(super) fn veins(&mut self, f: &Fields, part: &str, of: csg_sdf::SdfNode, bounds: (P3, P3)) -> Res<(csg_sdf::SdfNode, (P3, P3))> {
        use csg_sdf::SdfNode;
        let Some(lib) = self.texture_lib else { return f.err("the field library did not load") };
        let unit = self.unit;
        let Some(obj) = f.need("pattern")?.obj().cloned() else { return f.err("pattern is the slime mould whose veins it raises: Physarum{..}") };
        let pattern: PatternArg = self.grown(&obj)?;
        if pattern.kind != "Physarum" {
            return f.err(format!("veins grow from a slime mould, Physarum{{..}}, not {}", pattern.kind));
        }
        // asked, and told, once however often the part is lowered
        let first = !self.veins_requests.iter().any(|r| r.part == part);
        if first {
            self.veins_requests.push(super::VeinsRequest { part: part.into(), lo: bounds.0, hi: bounds.1, source: of.clone(), pattern });
        }
        let tubes: Vec<f32> = match self.solved.get(part) {
            Some(grid) => grid.table.to_vec(),
            None => {
                if first {
                    let (line, col) = f.obj.loc.unwrap_or((1, 1));
                    self.notes.push(Diag { line, col, message: format!("{part}: its veins rise once the slime mould has grown on its source") });
                }
                Vec::new()
            }
        };
        if tubes.len() % TUBE_WORDS != 0 {
            return f.err(format!("the host passed {} words, not {TUBE_WORDS} a tube", tubes.len()));
        }
        // no tubes: no solid, its box a point inside the source's
        let joined = |a: (P3, P3), b: (P3, P3)| (std::array::from_fn(|c| a.0[c].min(b.0[c])), std::array::from_fn(|c| a.1[c].max(b.1[c])));
        let b: (P3, P3) = tubes.chunks(TUBE_WORDS).map(solid_box).reduce(joined).unwrap_or_else(|| {
            let centre: P3 = std::array::from_fn(|c| 0.5 * (bounds.0[c] + bounds.1[c]));
            (centre, centre)
        });
        let grid = csg_sdf::SdfGrid::new([0.0; 3], 1.0, [2, 2, 2], 0.0, vec![0.0; 8].into()).with_table(table(&tubes, REACH * unit, BUCKET * unit).into());
        let func = self.texture_call(f, lib, "veins", &[ScriptValue::from_f64(unit)])?;
        let local = (b.0.map(|x| x / unit), b.1.map(|x| x / unit));
        let k = SdfKernel::splash(self.texture_kernel(f, func, Some(&grid))?, Some(local));
        Ok((SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale: unit, of: Box::new(SdfNode::Kernel(k)) }, b))
    }
}

/// A box holding a tube's solid (its twelve words: the round cone from `a`, radius `ra`, to `b`,
/// radius `rb`, on the side of the plane `dot(p, cut) = under` that `cut` points to). Along each
/// way `e` it reaches no farther than `h(e + λ cut) - λ under` for any `λ >= 0`, `h` the round
/// cone's support (its two balls' greatest), the least of which a golden section finds (it is
/// convex in `λ`): a ball's cap is as wide as its cut leaves it, not the ball.
fn solid_box(t: &[f32]) -> (P3, P3) {
    let w = |i: usize| t[i] as f64;
    let (a, ra, b, rb, cut, under) = ([w(0), w(1), w(2)], w(3), [w(4), w(5), w(6)], w(7), [w(8), w(9), w(10)], w(11));
    let dot = |u: P3, v: P3| u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let support = |v: P3| {
        let l = dot(v, v).sqrt();
        (dot(a, v) + ra * l).max(dot(b, v) + rb * l)
    };
    let reach = |e: P3| {
        let g = |l: f64| support(std::array::from_fn(|c| e[c] + l * cut[c])) - l * under;
        let k = 0.5 * (5f64.sqrt() - 1.0);
        let (mut lo, mut hi) = (0.0, 8.0);
        for _ in 0..60 {
            let (x, y) = (hi - k * (hi - lo), lo + k * (hi - lo));
            if g(x) <= g(y) {
                hi = y;
            } else {
                lo = x;
            }
        }
        g(0.5 * (lo + hi)).min(g(0.0))
    };
    let axis = |c: usize, s: f64| -> P3 { std::array::from_fn(|i| if i == c { s } else { 0.0 }) };
    (std::array::from_fn(|c| -reach(axis(c, -1.0))), std::array::from_fn(|c| reach(axis(c, 1.0))))
}

/// The table the `veins` kernel reads (the frame `vor_list` reads): origin xyz, 1 / bucket, bucket
/// counts xyz, the reach `reach`, the multiple lists start on, each bucket's list (that many
/// multiples from the table's start), an empty list, then the lists: a count and the tubes whose
/// box grown by `reach` meets the bucket, [`TUBE_WORDS`] words each. Lengths in the part's units.
/// A point off the grid reads its edge buckets, which are empty.
pub fn table(tubes: &[f32], reach: f64, bucket: f64) -> Vec<f32> {
    let count = tubes.len() / TUBE_WORDS;
    let tube = |i: usize| &tubes[i * TUBE_WORDS..(i + 1) * TUBE_WORDS];
    // each tube's box: its two balls'
    let boxed = |i: usize| -> (P3, P3) {
        let t = tube(i);
        let (ra, rb) = (t[3] as f64, t[7] as f64);
        (std::array::from_fn(|c| (t[c] as f64 - ra).min(t[4 + c] as f64 - rb) - reach), std::array::from_fn(|c| (t[c] as f64 + ra).max(t[4 + c] as f64 + rb) + reach))
    };
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for i in 0..count {
        let (a, b) = boxed(i);
        for c in 0..3 {
            lo[c] = lo[c].min(a[c]);
            hi[c] = hi[c].max(b[c]);
        }
    }
    if count == 0 {
        (lo, hi) = ([0.0; 3], [0.0; 3]);
    }
    let mut width = bucket;
    let dims = |width: f64| -> [usize; 3] { std::array::from_fn(|c| ((hi[c] - lo[c]) / width).ceil() as usize + 3) };
    while dims(width).iter().map(|n| *n as f64).product::<f64>() > MAX_BUCKETS {
        width *= 1.25;
    }
    let n = dims(width);
    let origin: P3 = std::array::from_fn(|c| lo[c] - width);
    let total = n[0] * n[1] * n[2];
    let mut lists: Vec<Vec<u32>> = vec![Vec::new(); total];
    for i in 0..count {
        let (a, b) = boxed(i);
        let first: [usize; 3] = std::array::from_fn(|c| ((a[c] - origin[c]) / width).floor() as usize);
        let last: [usize; 3] = std::array::from_fn(|c| ((b[c] - origin[c]) / width).floor() as usize);
        for z in first[2]..=last[2] {
            for y in first[1]..=last[1] {
                for x in first[0]..=last[0] {
                    lists[x + n[0] * (y + n[1] * z)].push(i as u32);
                }
            }
        }
    }
    let head = 9 + total;
    let filled = lists.iter().filter(|l| !l.is_empty()).count();
    let in_lists = 1 + lists.iter().map(|l| if l.is_empty() { 0 } else { 1 + TUBE_WORDS * l.len() }).sum::<usize>();
    let mut every = 1;
    while (head + in_lists + (filled + 1) * (every - 1)) / every > EXACT_WORD {
        every *= 2;
    }
    // a wrapped index would read another field, and that field would print
    assert!(head + in_lists + (filled + 1) * (every - 1) <= i32::MAX as usize, "{count} tubes are more than a kernel's int can index");
    let mut w: Vec<f32> = Vec::with_capacity(head + in_lists + (filled + 1) * (every - 1));
    w.extend(origin.map(|x| x as f32));
    w.push((1.0 / width) as f32);
    w.extend(n.map(|x| x as f32));
    w.push(reach as f32);
    w.push(every as f32);
    w.resize(head, 0.0);
    let start = |w: &mut Vec<f32>| -> f32 {
        w.resize(w.len().next_multiple_of(every), 0.0);
        (w.len() / every) as f32
    };
    let empty = start(&mut w);
    w[9..head].fill(empty);
    w.push(0.0);
    for (k, list) in lists.iter().enumerate().filter(|(_, l)| !l.is_empty()) {
        w[9 + k] = start(&mut w);
        w.push(list.len() as f32);
        for &i in list {
            w.extend_from_slice(tube(i as usize));
        }
    }
    w
}

//! A pass over many items spread over a pool's workers, with the result one pass on one thread
//! gives.

use makepad_widgets_core::makepad_platform::thread::TaskPool;
use std::sync::Mutex;

/// The share of a pass one task of the pool takes: at most 512 tasks, of at least 256 items, so
/// the workers that take them as they finish share uneven items (a plate's touching parts) evenly.
pub(crate) fn chunk_of(len: usize) -> usize {
    len.div_ceil(512).max(256)
}

/// `f` over `0..len`, a chunk of `chunk` at a time on the pool's workers (the caller helps), the
/// chunks' results one after the other: what one call over the whole range gives, as `f`'s
/// result for a range is its results for each index in turn. Without a pool, that one call.
pub(crate) fn in_chunks<R: Send>(pool: Option<&TaskPool>, len: usize, chunk: usize, f: impl Fn(std::ops::Range<usize>) -> Vec<R> + Sync) -> Vec<R> {
    let chunks = len.div_ceil(chunk.max(1));
    let Some(pool) = pool.filter(|_| chunks > 1) else { return f(0..len) };
    let out: Vec<Mutex<Vec<R>>> = (0..chunks).map(|_| Mutex::new(Vec::new())).collect();
    pool.fan_out(chunks, |i| *out[i].lock().unwrap() = f(i * chunk..((i + 1) * chunk).min(len)));
    let out: Vec<Vec<R>> = out.into_iter().map(|o| o.into_inner().unwrap()).collect();
    let mut all = Vec::with_capacity(out.iter().map(Vec::len).sum());
    for mut o in out {
        all.append(&mut o);
    }
    all
}

/// `items` in `cmp`'s order, which tells any two of them apart, so there is one: runs of them
/// sorted on the pool's workers, then merged a pair at a time. Without a pool, one sort.
pub(crate) fn sorted<T: Copy + Send + Sync>(pool: Option<&TaskPool>, mut items: Vec<T>, cmp: impl Fn(&T, &T) -> std::cmp::Ordering + Sync) -> Vec<T> {
    let run = items.len().div_ceil(16).max(256);
    let Some(pool) = pool.filter(|_| items.len() > run) else {
        items.sort_unstable_by(cmp);
        return items;
    };
    {
        let runs: Vec<Mutex<&mut [T]>> = items.chunks_mut(run).map(Mutex::new).collect();
        pool.fan_out(runs.len(), |i| runs[i].lock().unwrap().sort_unstable_by(&cmp));
    }
    let merge = |a: &[T], b: &[T]| {
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0, 0);
        while i < a.len() && j < b.len() {
            if cmp(&b[j], &a[i]).is_lt() {
                out.push(b[j]);
                j += 1;
            } else {
                out.push(a[i]);
                i += 1;
            }
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        out
    };
    // each level's merged runs, which the next level's slices borrow
    let mut runs: Vec<Vec<T>>;
    let mut slices: Vec<&[T]> = items.chunks(run).collect();
    loop {
        let pairs: Vec<Mutex<Vec<T>>> = (0..slices.len() / 2).map(|_| Mutex::new(Vec::new())).collect();
        pool.fan_out(pairs.len(), |i| *pairs[i].lock().unwrap() = merge(slices[2 * i], slices[2 * i + 1]));
        let odd = (slices.len() % 2 == 1).then(|| slices[slices.len() - 1].to_vec());
        runs = pairs.into_iter().map(|p| p.into_inner().unwrap()).chain(odd).collect();
        if runs.len() == 1 {
            return runs.pop().unwrap();
        }
        slices = runs.iter().map(Vec::as_slice).collect();
    }
}

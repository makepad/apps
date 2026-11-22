use fab_elec_sim::trace::{MinMax, SignalTrace, TimeAxis, TraceStore, CHUNK};

/// Finds the first sample whose time is at least `t`, or the length.
pub fn time_index(time: &TimeAxis, t: f64) -> usize {
    partition_time(time, |time| time < t)
}

/// Finds the first false value of a monotone predicate, searching chunk starts first.
pub fn partition_time(time: &TimeAxis, pred: impl Fn(f64) -> bool) -> usize {
    let chunks = time.chunks.partition_point(|chunk| pred(chunk[0]));
    if chunks == time.chunks.len() && time.open.first().is_some_and(|&time| pred(time)) {
        return chunks * CHUNK + time.open.partition_point(|&time| pred(time));
    }
    if chunks == 0 {
        return 0;
    }
    (chunks - 1) * CHUNK + time.chunks[chunks - 1].partition_point(|&time| pred(time))
}

/// Reads the extremes of `a..b` from chunk pyramids, scanning only unaligned ends, the chunks whose
/// pyramid is not built yet and the open tail.
pub fn range_minmax(signal: &SignalTrace, mut a: usize, b: usize) -> Option<MinMax> {
    if a >= b {
        return None;
    }
    let mut out = MinMax { min: f32::INFINITY, max: f32::NEG_INFINITY };
    let closed_end = signal.chunks.len() * CHUNK;
    while a < b.min(closed_end) {
        let chunk_index = a / CHUNK;
        let chunk = &signal.chunks[chunk_index];
        let end = b.min((chunk_index + 1) * CHUNK);
        let mut left = a % CHUNK;
        let right = end - chunk_index * CHUNK;
        let pyramid = chunk.pyramid().is_some();
        while left < right {
            if !pyramid || left % 4 != 0 || right - left < 4 {
                let value = chunk.y[left];
                out.min = out.min.min(value);
                out.max = out.max.max(value);
                left += 1;
                continue;
            }
            // Take the largest aligned bin that fits, in sample order.
            let mut width = 4;
            let mut level = 0;
            while width < CHUNK && left % (width * 4) == 0 && width * 4 <= right - left {
                width *= 4;
                level += 1;
            }
            let Some(bins) = chunk.level(level) else { break };
            let bin = bins[left / width];
            out.min = out.min.min(bin.min);
            out.max = out.max.max(bin.max);
            left += width;
        }
        a = end;
    }
    if a < b {
        for &value in &signal.open[a - closed_end..b - closed_end] {
            out.min = out.min.min(value);
            out.max = out.max.max(value);
        }
    }
    Some(out)
}

/// Interpolates at `t`, returning no value before the first sample and holding the last value.
pub fn value_at(store: &TraceStore, signal: usize, t: f64) -> Option<f32> {
    let signal = store.signals.get(signal)?;
    let len = store.len();
    if len == 0 || len != signal.chunks.len() * CHUNK + signal.open.len() || t.is_nan() || t < store.time.at(0) {
        return None;
    }
    let next = time_index(&store.time, t);
    if next == len {
        return Some(signal.at(len - 1));
    }
    let right = signal.at(next);
    let right_time = store.time.at(next);
    if next == 0 || t == right_time {
        return Some(right);
    }
    let left_time = store.time.at(next - 1);
    let left = f64::from(signal.at(next - 1));
    let fraction = (t - left_time) / (right_time - left_time);
    Some((left + (f64::from(right) - left) * fraction) as f32)
}

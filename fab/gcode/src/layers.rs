//! The print timeline per layer, for scrubbing and playback by true G-code
//! time: one chunk per G-code layer (chunk 0: the start sequence), its moves
//! timed from the chunk's own start, and the chunks' absolute starts.
//!
//! A chunk whose moves did not change keeps its `Arc` from the previous
//! timeline ([`LayerTimeline::from_timeline`] with `prev`), so a consumer
//! re-renders only the chunks that are new (`Arc::ptr_eq`) and re-bases the
//! rest through `start`: an edit low in the print moves every later start,
//! not every later move.

use crate::packed::PackedMoves;
use crate::process::Timeline;
use crate::record::MoveRecord;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct LayerChunk {
    /// 0: the start sequence; then the G-code's layer number.
    pub layer: u32,
    pub z: f32,
    /// Every record of the layer (extrusions, travels, retractions, tool
    /// changes, waits), `t_start`/`t_end` from the chunk's start, packed. Their
    /// `gcode_line` is 0 here: lines move with any edit above or below, so
    /// they live in [`LayerTimeline::lines`].
    pub moves: Arc<PackedMoves>,
    pub duration: f64,
    /// The content of `moves` (and z, duration): equal hashes, equal chunks.
    pub hash: u64,
}

#[derive(Clone, Debug, Default)]
pub struct LayerTimeline {
    pub chunks: Vec<Arc<LayerChunk>>,
    /// Absolute start of each chunk (s from the print's start).
    pub start: Vec<f64>,
    /// Per chunk, its records' G-code lines (absolute).
    pub lines: Vec<LineSteps>,
    pub total: f64,
    /// The nozzles' targets the records' `temps` name (the timeline's table).
    pub nozzle_temps: Arc<Vec<Vec<i16>>>,
}

/// A chunk's G-code lines, each as its step from the record's before (the first's from 0), in
/// zigzag varints: a record is a line or a few, so most steps take a byte.
#[derive(Clone, Debug, Default)]
pub struct LineSteps(Box<[u8]>);

impl LineSteps {
    fn of(lines: impl Iterator<Item = u32>, n: usize) -> LineSteps {
        let mut out = Vec::with_capacity(n);
        let mut last = 0i64;
        for line in lines {
            let d = line as i64 - last;
            last = line as i64;
            let mut z = ((d << 1) ^ (d >> 63)) as u64;
            while z >= 0x80 {
                out.push(z as u8 | 0x80);
                z >>= 7;
            }
            out.push(z as u8);
        }
        LineSteps(out.into_boxed_slice())
    }

    /// The lines, in the records' order.
    pub fn iter(&self) -> impl Iterator<Item = u32> + '_ {
        let (mut at, mut last) = (0, 0i64);
        std::iter::from_fn(move || {
            let (mut z, mut shift) = (0u64, 0);
            loop {
                let b = *self.0.get(at)?;
                at += 1;
                z |= ((b & 0x7f) as u64) << shift;
                shift += 7;
                if b < 0x80 {
                    break;
                }
            }
            last += (z >> 1) as i64 ^ -((z & 1) as i64);
            Some(last as u32)
        })
    }
}

fn hash_chunk(z: f32, duration: f64, moves: &[MoveRecord]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut eat = |x: u64| h = (h ^ x).wrapping_mul(0x100_0000_01b3);
    eat(z.to_bits() as u64);
    eat(duration.to_bits());
    for m in moves {
        eat(m.t_start.to_bits());
        eat(m.t_end.to_bits());
        eat(m.kind as u64);
        eat(m.role as u64);
        for c in m.p0.iter().chain(&m.p1).chain(&m.center) {
            eat(c.to_bits() as u64);
        }
        eat(((m.tool as u64) << 48) | ((m.nozzle as u64) << 40) | ((m.filament as u64) << 32) | m.flags as u64);
        for f in [m.width, m.height, m.e, m.v_entry, m.v_cruise, m.v_exit, m.accel, m.f_cmd, m.sweep, m.z_layer] {
            eat(f.to_bits() as u64);
        }
        eat(((m.fan_part as u64) << 16) | ((m.fan_aux as u64) << 8) | m.fan_chamber as u64);
        // The temperature entry's number: equal chunks of one timeline name equal targets.
        eat(((m.temps as u64) << 16) | m.t_bed as u16 as u64);
        eat(((m.block as u64) << 48) | ((m.object as u64) << 32));
    }
    h
}

impl LayerTimeline {
    /// Chunks from a timeline's records (in print order), reusing `prev`'s chunk for every layer
    /// whose content is unchanged. The records move here: `tl` ends up without any. One scan finds
    /// where each chunk starts (a record whose layer differs from the one before), then the
    /// chunks are made in parallel, a window of parts at a time, and each part is freed once every
    /// chunk reading it is made: the conversion never holds two copies of a big print.
    pub fn from_timeline(tl: &mut Timeline, prev: Option<&LayerTimeline>) -> LayerTimeline {
        let mut parts = std::mem::take(&mut tl.parts);
        // Each chunk's first record (part, index), its layer and z, and its time base.
        let mut heads: Vec<(usize, usize, u32, f32, f64)> = Vec::new();
        let mut last_end = 0.0f64;
        for (pi, part) in parts.iter().enumerate() {
            for (ri, m) in part.iter().enumerate() {
                if heads.last().is_none_or(|h| h.2 != m.layer) {
                    let t0 = if heads.is_empty() { 0.0 } else { m.t_start };
                    heads.push((pi, ri, m.layer, m.z_layer, t0));
                }
                last_end = m.t_end;
            }
        }
        let mut out = LayerTimeline { total: tl.total.max(last_end), nozzle_temps: Arc::new(std::mem::take(&mut tl.nozzle_temps)), ..Default::default() };
        let total = out.total;
        let window = makepad_fab_toolpath::par::threads().max(1);
        let mut k0 = 0;
        while k0 < heads.len() {
            let k1 = (k0 + 1..heads.len()).find(|&k| heads[k].0 >= heads[k0].0 + window).unwrap_or(heads.len());
            let made = makepad_fab_toolpath::par::map(k1 - k0, |i| {
                let k = k0 + i;
                let (p0, r0, layer, z, t0) = heads[k];
                let (p1, r1) = heads.get(k + 1).map_or((parts.len(), 0), |h| (h.0, h.1));
                let pieces = (p0..parts.len().min(p1 + 1)).map(|p| &parts[p][if p == p0 { r0 } else { 0 }..if p == p1 { r1 } else { parts[p].len() }]);
                let n = pieces.clone().map(|r| r.len()).sum();
                let lines = LineSteps::of(pieces.clone().flatten().map(|m| m.gcode_line), n);
                let mut rel = Vec::with_capacity(n);
                for m in pieces.flatten() {
                    rel.push(MoveRecord { t_start: m.t_start - t0, t_end: m.t_end - t0, gcode_line: 0, ..*m });
                }
                let duration = heads.get(k + 1).map_or(total, |h| h.4) - t0;
                let hash = hash_chunk(z, duration, &rel);
                let reuse = prev.and_then(|p| p.chunks.get(k)).filter(|c| c.hash == hash && c.layer == layer).cloned();
                (reuse.unwrap_or_else(|| Arc::new(LayerChunk { layer, z, moves: Arc::new(PackedMoves::pack(&rel)), duration, hash })), t0, lines)
            });
            for (chunk, t0, lines) in made {
                out.chunks.push(chunk);
                out.start.push(t0);
                out.lines.push(lines);
            }
            // Every part before the next chunk's first is read.
            let done = heads.get(k1).map_or(parts.len(), |h| h.0);
            for part in &mut parts[..done] {
                *part = Vec::new();
            }
            k0 = k1;
        }
        out
    }

    /// Every record in print order, on the print's clock with its G-code line.
    pub fn records(&self) -> impl Iterator<Item = MoveRecord> + '_ {
        self.chunks.iter().zip(&self.start).zip(&self.lines).flat_map(|((c, &t0), lines)| {
            c.moves.iter().zip(lines.iter()).map(move |(m, gcode_line)| MoveRecord { t_start: m.t_start + t0, t_end: m.t_end + t0, gcode_line, ..m })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(t0: f64, t1: f64, layer: u32, x: f32, line: u32) -> MoveRecord {
        MoveRecord { t_start: t0, t_end: t1, layer, p0: [x, 0.0, 0.0], p1: [x + 1.0, 0.0, 0.0], gcode_line: line, ..Default::default() }
    }

    /// An edit in layer 1 that makes it longer re-bases layer 2, which
    /// keeps its chunk; the edited layer gets a new one.
    #[test]
    fn unchanged_layers_keep_their_chunks() {
        let mut a = Timeline { parts: vec![vec![rec(0.0, 1.0, 0, 0.0, 1), rec(1.0, 3.0, 1, 1.0, 5), rec(3.0, 4.0, 2, 2.0, 9)]], total: 4.0, ..Default::default() };
        let mut b = Timeline { parts: vec![vec![rec(0.0, 1.0, 0, 0.0, 1), rec(1.0, 5.0, 1, 1.5, 5), rec(5.0, 6.0, 2, 2.0, 9)]], total: 6.0, ..Default::default() };
        let b_records = b.parts.concat();
        let ta = LayerTimeline::from_timeline(&mut a, None);
        let tb = LayerTimeline::from_timeline(&mut b, Some(&ta));
        // A packed chunk reads back its records bit for bit, a jump (layer 1 starts off where
        // layer 0 ended) and G-code lines included.
        assert_eq!(tb.records().collect::<Vec<_>>(), b_records, "the records read back as they were");
        assert!(Arc::ptr_eq(&ta.chunks[0], &tb.chunks[0]));
        assert!(!Arc::ptr_eq(&ta.chunks[1], &tb.chunks[1]));
        assert!(Arc::ptr_eq(&ta.chunks[2], &tb.chunks[2]), "layer 2 only moved in time");
        assert_eq!(tb.start, vec![0.0, 1.0, 5.0]);
    }
}

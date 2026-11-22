//! Musical time for the audio-sync tools (EDITS.md §10.3): a beat grid and
//! snapping block edges to it. Pure.

/// A straight tempo line: beat `n` is at `offset + n * 60 / bpm`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub bpm: f64,
    /// Where beat 0 (a downbeat) is, seconds.
    pub offset: f64,
    pub beats_per_bar: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unit {
    HalfBeat,
    Beat,
    /// The first beat of a bar (a downbeat); the same points as `Bar`.
    Downbeat,
    Bar,
}

impl Unit {
    pub fn from_id(id: &str) -> Option<Unit> {
        Some(match id {
            "half_beat" => Unit::HalfBeat,
            "beat" => Unit::Beat,
            "downbeat" => Unit::Downbeat,
            "bar" => Unit::Bar,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edges {
    Start,
    End,
    Both,
}

impl Edges {
    pub fn from_id(id: &str) -> Option<Edges> {
        Some(match id {
            "start" => Edges::Start,
            "end" => Edges::End,
            "both" => Edges::Both,
            _ => return None,
        })
    }
}

impl Grid {
    pub fn beat_len(&self) -> f64 {
        60.0 / self.bpm.max(1e-6)
    }

    pub fn step(&self, unit: Unit) -> f64 {
        match unit {
            Unit::HalfBeat => self.beat_len() / 2.0,
            Unit::Beat => self.beat_len(),
            Unit::Downbeat | Unit::Bar => self.beat_len() * self.beats_per_bar.max(1) as f64,
        }
    }

    /// The grid point of `unit` nearest `t` (never before 0).
    pub fn snap(&self, t: f64, unit: Unit) -> f64 {
        let step = self.step(unit);
        let n = ((t - self.offset) / step).round();
        (self.offset + n * step).max(0.0)
    }

    /// Beat times in `[0, until]`, with whether each is a downbeat.
    pub fn beats(&self, until: f64) -> Vec<(f64, bool)> {
        let beat = self.beat_len();
        let first = ((0.0 - self.offset) / beat).ceil() as i64;
        let mut out = Vec::new();
        let mut n = first;
        loop {
            let t = self.offset + n as f64 * beat;
            if t > until + 1e-9 {
                break;
            }
            out.push((t, n.rem_euclid(self.beats_per_bar.max(1) as i64) == 0));
            n += 1;
        }
        out
    }

    /// Whole bars nearest `duration` (at least one), and their length.
    pub fn whole_bars(&self, duration: f64) -> (u32, f64) {
        let bar = self.step(Unit::Bar);
        let bars = ((duration - self.offset.max(0.0)) / bar).round().max(1.0) as u32;
        (bars, self.offset.max(0.0) + bars as f64 * bar)
    }
}

/// New spans for blocks `(id, start, length)` with their edges on the
/// grid; only the ones that move. A block never shrinks below one step.
pub fn snap_spans(blocks: &[(String, f64, f64)], grid: &Grid, unit: Unit, edges: Edges) -> Vec<(String, f64, f64)> {
    let step = grid.step(unit);
    blocks.iter().filter_map(|(id, start, length)| {
        let end = start + length;
        let (mut new_start, mut new_end) = (*start, end);
        if matches!(edges, Edges::Start | Edges::Both) {
            new_start = grid.snap(*start, unit);
        }
        if matches!(edges, Edges::End | Edges::Both) {
            new_end = grid.snap(end, unit);
        }
        if matches!(edges, Edges::Start) {
            // Moving the start keeps the length.
            new_end = new_start + length;
        }
        if !matches!(edges, Edges::Start) && new_end - new_start < step - 1e-9 {
            new_end = new_start + step;
        }
        let moved = (new_start - start).abs() > 1e-6 || (new_end - end).abs() > 1e-6;
        moved.then(|| (id.clone(), new_start, new_end - new_start))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid() -> Grid {
        Grid { bpm: 120.0, offset: 0.0, beats_per_bar: 4 }
    }

    #[test]
    fn snapping_to_beats_and_bars() {
        let grid = grid();
        assert_eq!(grid.snap(1.26, Unit::Beat), 1.5);
        assert_eq!(grid.snap(1.2, Unit::Beat), 1.0);
        assert_eq!(grid.snap(2.9, Unit::Bar), 2.0);
        assert_eq!(grid.snap(3.1, Unit::Downbeat), 4.0);
        assert_eq!(grid.snap(0.3, Unit::HalfBeat), 0.25);
        let offset = Grid { offset: 0.1, ..grid };
        assert!((offset.snap(0.0, Unit::Beat) - 0.1).abs() < 1e-9);
    }

    #[test]
    fn beats_mark_downbeats_and_bars_fit() {
        let beats = grid().beats(2.0);
        assert_eq!(beats.len(), 5);
        assert!(beats[0].1 && !beats[1].1 && !beats[3].1 && beats[4].1);
        assert_eq!(grid().whole_bars(17.0), (9, 18.0));
        assert_eq!(grid().whole_bars(0.1), (1, 2.0));
    }

    #[test]
    fn spans_snap_and_never_collapse() {
        let blocks = vec![("a".to_string(), 0.1, 1.8), ("b".to_string(), 2.0, 2.0), ("c".to_string(), 3.1, 0.1)];
        let moved = snap_spans(&blocks, &grid(), Unit::Beat, Edges::Both);
        assert_eq!(moved, vec![("a".to_string(), 0.0, 2.0), ("c".to_string(), 3.0, 0.5)]);
        let starts = snap_spans(&blocks, &grid(), Unit::Bar, Edges::Start);
        assert_eq!(starts[0], ("a".to_string(), 0.0, 1.8));
    }
}

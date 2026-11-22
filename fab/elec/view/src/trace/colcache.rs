//! A lane's reduced columns kept between frames (the Scope's take, L3): a frame where only the
//! play head moved reads nothing, and a record that grew reduces only the columns from its old
//! end on, equal to a full reduction.

use super::columns::Columns;
use fab_elec_sim::trace::TraceStore;

/// Number of full reductions, partial reductions and cache hits.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub full: u64,
    pub partial: u64,
    pub reused: u64,
}

#[derive(PartialEq, Eq)]
struct Key {
    signal: usize,
    t0_bits: u64,
    t1_bits: u64,
    cols: u32,
    generation: u64,
    rewinds: u64,
    folds_made: u64,
    /// A rolling record's dropped front, and the leading positions thinned to the whole signals.
    base: usize,
    thinned: usize,
}

/// One lane's reduced columns, kept between frames.
#[derive(Default)]
pub struct ColumnCache {
    key: Option<Key>,
    len: usize,
    columns: Columns,
    stats: CacheStats,
}

impl ColumnCache {
    /// Starts with no kept columns.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the same columns as a full reduction of the signal over `[t0, t1)`.
    pub fn columns(&mut self, store: &TraceStore, signal: usize, t0: f64, t1: f64, cols: u32) -> &Columns {
        let key = Key {
            signal,
            t0_bits: t0.to_bits(),
            t1_bits: t1.to_bits(),
            cols,
            generation: store.generation,
            rewinds: store.rewinds,
            folds_made: store.folds_made,
            base: store.base,
            thinned: store.thinned(),
        };
        let len = store.time.len();
        if self.key.as_ref() == Some(&key) {
            if len == self.len {
                self.stats.reused += 1;
                return &self.columns;
            }
            if len > self.len && self.columns.reduce_trace_suffix(store, signal, self.len) {
                self.len = len;
                self.stats.partial += 1;
                return &self.columns;
            }
        }
        self.columns.reduce_trace_into(store, signal, t0, t1, cols);
        self.key = Some(key);
        self.len = len;
        self.stats.full += 1;
        &self.columns
    }

    /// Returns cumulative counts for this lane.
    pub fn stats(&self) -> CacheStats {
        self.stats
    }

    /// Forgets the kept columns so the next call reduces in full.
    pub fn clear(&mut self) {
        self.key = None;
    }
}

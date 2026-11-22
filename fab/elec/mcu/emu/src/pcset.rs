//! Dense marks in a machine's native code-address unit.

use std::sync::atomic::{AtomicU64, Ordering};

/// The next stamp a set takes when it changes.
static STAMPS: AtomicU64 = AtomicU64::new(1);

/// Code addresses over `[base, base + len)`, one bit per address unit (AVR
/// flash words, Xtensa bytes). Addresses outside the range are not contained.
///
/// Each set carries a stamp: a set takes a fresh one at every insert and a clone keeps it, so
/// two sets with one stamp hold the same addresses. An empty set's stamp is 0.
#[derive(Clone, Debug, Default)]
pub struct PcSet { base: u32, len: u32, bits: Vec<u64>, stamp: u64 }

impl PcSet {
    /// Creates an empty set; panics if the range exceeds the code address space.
    pub fn new(base: u32, len: u32) -> PcSet {
        assert!(u64::from(base) + u64::from(len) <= 1u64 << 32, "code range exceeds u32");
        PcSet { base, len, bits: vec![0; ((u64::from(len) + 63) / 64) as usize], stamp: 0 }
    }

    /// Marks `pc`; panics outside the configured range.
    pub fn insert(&mut self, pc: u32) {
        let offset = pc.checked_sub(self.base).filter(|&i| i < self.len)
            .expect("marked PC outside range");
        self.bits[(offset / 64) as usize] |= 1u64 << (offset % 64);
        self.stamp = STAMPS.fetch_add(1, Ordering::Relaxed);
    }

    /// Tests a mark with one bitmap load and one bit test after the range check.
    #[inline]
    pub fn contains(&self, pc: u32) -> bool {
        let Some(offset) = pc.checked_sub(self.base).filter(|&i| i < self.len) else { return false; };
        self.bits[(offset / 64) as usize] & (1u64 << (offset % 64)) != 0
    }

    /// Whether no addresses have been marked.
    pub fn is_empty(&self) -> bool { self.bits.iter().all(|&bits| bits == 0) }

    /// Equal stamps mean equal sets (see the type's doc); 0 is an empty set.
    pub fn stamp(&self) -> u64 { self.stamp }

    /// The marked addresses in `[lo, hi)`, ascending.
    pub fn marks_in(&self, lo: u32, hi: u32) -> impl Iterator<Item = u32> + '_ {
        let base = u64::from(self.base);
        let start = u64::from(lo).max(base) - base;
        let end = u64::from(hi).min(base + u64::from(self.len)).saturating_sub(base);
        let words = if end > start { start / 64..end.div_ceil(64) } else { 0..0 };
        words.flat_map(move |w| {
            let mut word = self.bits[w as usize];
            std::iter::from_fn(move || {
                if word == 0 { return None; }
                let bit = word.trailing_zeros();
                word &= word - 1;
                Some(w * 64 + u64::from(bit))
            })
        }).filter(move |&offset| offset >= start && offset < end).map(move |offset| self.base + offset as u32)
    }
}

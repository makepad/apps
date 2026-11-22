use std::collections::BTreeMap;
use crate::{FaultCounts, Faults};

#[derive(Clone)]
pub(crate) struct Prng(u64);

impl Prng {
    pub fn new(seed: u64) -> Self { Self(seed) }
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        z ^ (z >> 31)
    }
    fn chance(&mut self, ppm: u32) -> bool { ppm != 0 && self.next() % 1_000_000 < u64::from(ppm) }
}

#[derive(Clone, Default)]
pub(crate) struct Link {
    due: BTreeMap<(u64, u64), Vec<u8>>,
    held: Option<Held>,
    serial: u64,
}

#[derive(Clone)]
struct Held { ready: u64, expires: u64, frame: Vec<u8> }

impl Link {
    fn queue(&mut self, at: u64, frame: Vec<u8>) {
        self.due.insert((at, self.serial), frame);
        self.serial = self.serial.wrapping_add(1);
    }

    pub fn inject(&mut self, now: u64, frame: Vec<u8>, faults: &Faults, rng: &mut Prng, stats: &mut FaultCounts) {
        self.expire(now);
        if rng.chance(faults.drop_ppm) { stats.dropped += 1; return; }
        let copies = if rng.chance(faults.dup_ppm) { stats.duplicated += 1; 2 } else { 1 };
        for _ in 0..copies {
            let delay = if faults.delay_max_ms == 0 { 0 } else { rng.next() % (u64::from(faults.delay_max_ms) + 1) };
            if delay != 0 { stats.delayed += 1; }
            let due = now.saturating_add(delay);
            if let Some(held) = self.held.take() {
                self.queue(due, frame.clone());
                // Serial order puts the held frame after its successor even
                // when their sampled delays give them the same due time.
                self.queue(due.max(held.ready), held.frame);
            } else if rng.chance(faults.reorder_ppm) {
                stats.reordered += 1;
                // A lone last packet must not wait forever for a successor.
                self.held = Some(Held { ready: due, expires: due.saturating_add(40), frame: frame.clone() });
            } else { self.queue(due, frame.clone()); }
        }
    }

    fn expire(&mut self, now: u64) {
        if self.held.as_ref().is_some_and(|held| held.expires <= now) {
            let held = self.held.take().unwrap();
            self.queue(held.expires, held.frame);
        }
    }

    pub fn take(&mut self, now: u64) -> Vec<Vec<u8>> {
        self.expire(now);
        let mut out = Vec::new();
        while self.due.first_key_value().is_some_and(|(&(at, _), _)| at <= now) {
            out.push(self.due.pop_first().unwrap().1);
        }
        out
    }

    /// Forget every queued and held frame: they belong to a device that reset.
    pub fn clear(&mut self) {
        self.due.clear();
        self.held = None;
    }

    pub fn deadline(&self) -> Option<u64> {
        self.due.first_key_value().map(|(&(at, _), _)| at)
            .into_iter().chain(self.held.as_ref().map(|held| held.expires)).min()
    }
}

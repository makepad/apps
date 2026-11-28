//! The UI samples the peer's playout meter independently of call-row snapshots.
use super::{ParticipantShown, Phase};
use makepad_town_core::key::PublicKey;
use std::{cell::Cell, collections::BTreeMap, rc::Rc};

pub(super) const HZ: u64 = 30;
pub(super) const STEPS: u8 = 16;

fn quantise(rms: f32) -> u8 {
    // Give normal speaking levels more of the arc; silence has no ink.
    (rms.clamp(0.0, 1.0).sqrt() * f32::from(STEPS)).round() as u8
}

#[derive(Default)]
pub(super) struct PeerLevel {
    pub step: Rc<Cell<u8>>,
    peer_levels: BTreeMap<PublicKey, f32>,
    last_slot: Option<u64>,
}

impl PeerLevel {
    pub fn clear(&mut self) {
        self.peer_levels.clear();
        self.step.set(0);
        self.last_slot = None;
    }

    pub fn peer_level(&self, peer: &PublicKey) -> f32 {
        self.peer_levels.get(peer).copied().unwrap_or_default()
    }

    pub fn max_peer_level(&self) -> f32 {
        self.peer_levels.values().copied().fold(0.0, f32::max)
    }

    pub fn remove_peer(&mut self, peer: &PublicKey) {
        if self.peer_levels.remove(peer).is_some() { self.step.set(quantise(self.max_peer_level())); }
    }

    pub fn sample<I>(&mut self, phase: Phase, peers: &[ParticipantShown], now_us: u64,
        read: impl FnOnce() -> I) where I: IntoIterator<Item = (PublicKey, f32)> {
        if phase != Phase::Live || peers.iter().all(|p| p.muted) { self.clear(); return }
        self.peer_levels.retain(|key, _| peers.binary_search_by_key(key, |p| p.peer).is_ok_and(|i| !peers[i].muted));
        self.step.set(quantise(self.max_peer_level()));
        let slot = now_us.saturating_mul(HZ) / 1_000_000;
        if self.last_slot == Some(slot) { return }
        self.last_slot = Some(slot);
        for (peer, raw) in read() {
            let level = self.peer_levels.entry(peer).and_modify(|prev| *prev = (*prev + raw) * 0.5).or_insert(raw);
            if *level < 0.0005 { *level = 0.0; }
        }
        self.step.set(quantise(self.max_peer_level()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_peer_meter_only_samples_live_playout_and_orders_its_steps() {
        let steps: Vec<_> = (0..=1024).map(|i| quantise(i as f32 / 1024.)).collect();
        assert_eq!((steps[0], steps[1024]), (0, STEPS), "silence and full scale span the arc");
        assert!(steps.windows(2).all(|w| w[0] <= w[1]), "more energy never shortens the arc");
        assert!((0..=STEPS).all(|step| steps.contains(&step)), "each ring length is reachable");

        let mut meter = PeerLevel::default();
        let reads = Cell::new(0);
        let peer = [1; 65];
        let mut peers = [ParticipantShown { peer, name: String::new(), muted: false, transcribing: false }];
        let read = || { reads.set(reads.get() + 1); [(peer, 1.0)] };
        for phase in [Phase::Idle, Phase::Calling, Phase::Ringing] {
            meter.sample(phase, &peers, 0, read);
        }
        assert_eq!(reads.get(), 0, "a call that is not live never reads the peer level");
        for us in (0..1_000_000).step_by(1_000) { meter.sample(Phase::Live, &peers, us, read); }
        assert_eq!(reads.get(), HZ, "busy UI traffic still samples only once per tick");
        peers[0].muted = true;
        meter.sample(Phase::Live, &peers, 1_000_000, read);
        assert_eq!(meter.step.get(), 0, "mute immediately masks the last full-scale reading");
        meter.sample(Phase::Idle, &peers, 2_000_000, read);
        assert_eq!(reads.get(), HZ, "mute and hang-up perform no further level reads");
    }
}

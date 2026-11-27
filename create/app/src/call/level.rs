//! The UI samples the peer's playout meter independently of call-row snapshots.
use super::Phase;
use std::{cell::Cell, rc::Rc};

pub(super) const HZ: u64 = 30;
pub(super) const STEPS: u8 = 16;

fn quantise(rms: f32) -> u8 {
    // Give normal speaking levels more of the arc; silence has no ink.
    (rms.clamp(0.0, 1.0).sqrt() * f32::from(STEPS)).round() as u8
}

#[derive(Default)]
pub(super) struct PeerLevel {
    pub step: Rc<Cell<u8>>,
    last_slot: Option<u64>,
}

impl PeerLevel {
    pub fn clear(&mut self) {
        self.step.set(0);
        self.last_slot = None;
    }

    pub fn sample(&mut self, phase: Phase, muted: bool, now_us: u64, read: impl FnOnce() -> f32) {
        if phase != Phase::Live || muted {
            self.clear();
            return;
        }
        let slot = now_us.saturating_mul(HZ) / 1_000_000;
        if self.last_slot != Some(slot) {
            self.last_slot = Some(slot);
            self.step.set(quantise(read()));
        }
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
        let read = || { reads.set(reads.get() + 1); 1.0 };
        for phase in [Phase::Idle, Phase::Calling, Phase::Ringing] {
            meter.sample(phase, false, 0, read);
        }
        assert_eq!(reads.get(), 0, "a call that is not live never reads the peer level");
        for us in (0..1_000_000).step_by(1_000) { meter.sample(Phase::Live, false, us, read); }
        assert_eq!(reads.get(), HZ, "busy UI traffic still samples only once per tick");
        meter.sample(Phase::Live, true, 1_000_000, read);
        assert_eq!(meter.step.get(), 0, "mute immediately masks the last full-scale reading");
        meter.sample(Phase::Idle, false, 2_000_000, read);
        assert_eq!(reads.get(), HZ, "mute and hang-up perform no further level reads");
    }
}

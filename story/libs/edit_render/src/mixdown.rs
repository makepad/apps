//! The export's sound through the audio engine: the cut's mix
//! ([`AudioMix`]) fed into a deck of a one-deck flow (`AudioDeck` into an
//! `AudioOutput`, the same engine, deck and feed the CUT page's preview
//! plays through), rendered offline at 48 kHz on the calling thread.
//!
//! Deterministic and exact: the feed plays without a dip at the feed's own
//! rate (each output sample is the fed sample), the output's gate is open
//! before the first sample (a silent pre-roll), and nothing reads a clock;
//! so the render is the mix, sample for sample, run after run.

use makepad_story_cut::mix::{AudioMix, SAMPLE_RATE};
use makepad_stage_live_audio::embedded::{embedded, EmbeddedAudio};
use makepad_stage_live_audio::feed::FeedRing;
use makepad_stage_live_audio::{AudioEngine, DeckCommand};
use std::sync::Arc;

const FLOW: &str = r#"
use mod.flow.*
let deck = AudioDeck{ play: 1 speed: 1 }
let out = AudioOutput{ enabled: 1 audio: deck.out(@audio) }
Flow{ label: "Mixdown" deck, out }
"#;

/// Frames rendered at a time (and fed one ahead of).
const PIECE: usize = 8192;
/// Silent frames that open the output's gate before the first sample.
const PRE_ROLL: usize = 1024;

pub struct Mixdown {
    mix: Arc<AudioMix>,
    host: EmbeddedAudio,
    engine: AudioEngine,
    ring: Arc<FeedRing>,
    /// The output sample the next render plays, and the first not fed yet.
    next: u64,
    fed: u64,
}

impl Mixdown {
    pub fn new(mix: Arc<AudioMix>) -> Result<Self, String> {
        let (mut host, mut engine) = embedded(FLOW, "edit/mixdown.splash", Vec::new())?;
        let ring = FeedRing::new(PIECE * 2 + 2);
        let rate = SAMPLE_RATE as f64;
        let (mut left, mut right) = (vec![0.0f32; PRE_ROLL], vec![0.0f32; PRE_ROLL]);
        // The device rate is known after a first render; the flow is
        // prepared, installed, and the feed loaded and running.
        engine.render(rate, &mut left[..1], &mut right[..1]);
        host.pump()?;
        host.load("deck", DeckCommand::LoadFeed { sequence: 1, ring: ring.clone(), ramp: false })?;
        for _ in 0..4 {
            engine.render(rate, &mut left, &mut right);
            host.pump()?;
        }
        if host.deck("deck").is_none_or(|deck| deck.deck.sequence != 1) {
            return Err("the mixdown's deck did not take its feed".into());
        }
        Ok(Self { mix, host, engine, ring, next: 0, fed: 0 })
    }

    /// `count` stereo frames of the mix from output sample `first`,
    /// interleaved i16, at the mix's gain (as [`AudioMix::mix`]).
    pub fn mix(&mut self, first: u64, count: usize) -> Vec<i16> {
        if first != self.next {
            self.ring.restart();
            self.next = first;
            self.fed = first;
        }
        let gain = self.mix.gain as f32;
        let mut out = Vec::with_capacity(count * 2);
        let (mut left, mut right) = (vec![0.0f32; PIECE], vec![0.0f32; PIECE]);
        let mut done = 0usize;
        while done < count {
            let n = (count - done).min(PIECE);
            // The deck reads a frame and the one after it: fed one ahead.
            let need = self.next + n as u64 + 1;
            if need > self.fed {
                let mut block = self.mix.mix_f32(self.fed, (need - self.fed) as usize);
                for v in &mut block {
                    *v = (*v * gain).clamp(-1.0, 1.0);
                }
                self.ring.push(&block);
                self.fed = need;
            }
            self.engine.render(SAMPLE_RATE as f64, &mut left[..n], &mut right[..n]);
            for i in 0..n {
                out.push((left[i] * 32767.0).round() as i16);
                out.push((right[i] * 32767.0).round() as i16);
            }
            self.next += n as u64;
            done += n;
            let _ = self.host.pump();
        }
        out
    }
}

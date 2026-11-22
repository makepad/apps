use crate::prbs::Prbs;
use crate::synth::{Spread, Synth};

/// A PWM or PRBS source replayed by an acquisition member.
#[derive(Clone, Debug)]
pub enum Signal {
    Synth(Synth),
    Prbs(Prbs),
}

impl Signal {
    /// Apply independent member variation. A PRBS has no component spread.
    pub fn member(&self, spread: Spread, index: u64) -> Signal {
        match self {
            Self::Synth(synth) => Self::Synth(synth.member(spread, index)),
            Self::Prbs(prbs) => Self::Prbs(prbs.member(index)),
        }
    }

    /// One whole source period in seconds. PRBS replays end on a pattern boundary,
    /// keeping the bit sequence continuous across the replay seam.
    pub fn period(&self) -> f64 {
        match self {
            Self::Synth(synth) => synth.period(),
            Self::Prbs(prbs) => ((1_u64 << prbs.order) - 1) as f64 * prbs.ui(),
        }
    }

    /// Sample the source with its adaptive time steps.
    pub fn record(&self, n: usize) -> (Vec<f64>, Vec<f32>) {
        match self {
            Self::Synth(synth) => synth.record(n),
            Self::Prbs(prbs) => prbs.record(n),
        }
    }

    /// Whether every parameter is in its domain for members drawn with `spread`: finite values,
    /// positive rates and level spans, damping under critical for every member. A PRBS of any order
    /// the generator runs (7, 9, 15, 23, 31) passes; a caller that pre-generates whole patterns
    /// limits the order further.
    pub fn valid(&self, spread: Spread) -> bool {
        let spread_ok = [spread.wn, spread.zeta, spread.height].iter().all(|v| v.is_finite() && *v >= 0.0);
        spread_ok && match self {
            Self::Synth(synth) => {
                synth.freq.is_finite()
                    && synth.freq > 0.0
                    && synth.wn.is_finite()
                    && synth.wn > 0.0
                    && synth.zeta.is_finite()
                    && synth.zeta > 0.0
                    && synth.zeta < 1.0
                    && synth.zeta * (4.0 * spread.zeta).exp() < 1.0
                    && synth.duty.is_finite()
                    && (0.0..=1.0).contains(&synth.duty)
                    && synth.lo.is_finite()
                    && synth.hi.is_finite()
                    && synth.hi > synth.lo
                    && synth.noise.is_finite()
                    && synth.noise >= 0.0
                    && (0.0..0.25).contains(&synth.wobble)
                    && (0.0..=1.0).contains(&synth.runt)
                    && synth.xtalk.is_finite()
                    && synth.xtalk >= 0.0
                    && (synth.xtalk == 0.0 || (synth.xtalk_freq.is_finite() && synth.xtalk_freq > 0.0))
            }
            Self::Prbs(prbs) => {
                prbs.bit_rate.is_finite()
                    && prbs.bit_rate > 0.0
                    && matches!(prbs.order, 7 | 9 | 15 | 23 | 31)
                    && prbs.lo.is_finite()
                    && prbs.hi.is_finite()
                    && prbs.hi > prbs.lo
                    && prbs.wn.is_finite()
                    && prbs.wn > 0.0
                    && prbs.zeta.is_finite()
                    && prbs.zeta > 0.0
                    && prbs.zeta < 1.0
                    && prbs.jitter.is_finite()
                    && prbs.jitter >= 0.0
                    && prbs.noise.is_finite()
                    && prbs.noise >= 0.0
            }
        } && self.period().is_finite() && self.period() > 0.0
    }

    /// The seed used for deterministic source variation.
    pub fn seed(&self) -> u64 {
        match self {
            Self::Synth(synth) => synth.seed,
            Self::Prbs(prbs) => prbs.seed,
        }
    }
}

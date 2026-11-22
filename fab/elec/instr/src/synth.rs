use std::f64::consts::{PI, TAU};

/// Ringing frequency of a coupled clock burst in Hz.
pub const XTALK_RING: f64 = 120e6;
/// Envelope decay time of a coupled clock burst in seconds.
pub const XTALK_DECAY: f64 = 4e-9;

// Each Gaussian reserves two independent streams.
const HEIGHT_STREAM: u64 = 0;
const RUNT_CHOICE_STREAM: u64 = 2;
const RUNT_HEIGHT_STREAM: u64 = 3;
const XTALK_PHASE_STREAM: u64 = 4;
const MEMBER_WN_STREAM: u64 = 5;
const MEMBER_ZETA_STREAM: u64 = 7;
const MEMBER_HEIGHT_STREAM: u64 = 9;

/// RMS relative deviations between members.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spread {
    pub wn: f64,
    pub zeta: f64,
    pub height: f64,
}

/// A ringing PWM source with deterministic noise.
#[derive(Clone, Debug, PartialEq)]
pub struct Synth {
    /// PWM frequency in Hz.
    pub freq: f64,
    /// High fraction of the period, from zero to one.
    pub duty: f64,
    pub lo: f64,
    pub hi: f64,
    /// Natural angular frequency in radians per second.
    pub wn: f64,
    /// Damping ratio, strictly between zero and one.
    pub zeta: f64,
    /// RMS noise in volts; zero disables noise.
    pub noise: f64,
    /// RMS relative variation of each pulse's height.
    pub wobble: f64,
    /// Chance of a pulse with 55% to 80% of its usual height.
    pub runt: f64,
    /// Peak coupled clock voltage; zero disables coupling.
    pub xtalk: f64,
    /// Coupled clock frequency in Hz, ignored when coupling is zero.
    pub xtalk_freq: f64,
    pub seed: u64,
}

impl Synth {
    /// A 20 kHz, 30% duty, 3.3 V source with 25 MHz ringing and 8 mV noise.
    pub fn pwm() -> Self {
        let zeta: f64 = 0.12;
        Self {
            freq: 20_000.0,
            duty: 0.3,
            lo: 0.0,
            hi: 3.3,
            wn: TAU * 25e6 / (1.0 - zeta * zeta).sqrt(),
            zeta,
            noise: 0.008,
            wobble: 0.0,
            runt: 0.0,
            xtalk: 0.0,
            xtalk_freq: 0.0,
            seed: 1,
        }
    }

    /// PWM period in seconds.
    pub fn period(&self) -> f64 {
        1.0 / self.freq
    }

    /// Apply fixed component tolerances and an independent member seed.
    pub fn member(&self, spread: Spread, index: u64) -> Synth {
        let mut member = Self {
            seed: self.seed + index + 1,
            ..self.clone()
        };
        if spread.wn != 0.0 {
            member.wn *= (spread.wn * gaussian(member.seed, 0, MEMBER_WN_STREAM)).exp();
        }
        if spread.zeta != 0.0 {
            member.zeta *= (spread.zeta * gaussian(member.seed, 0, MEMBER_ZETA_STREAM)).exp();
        }
        if spread.height != 0.0 {
            member.hi = self.lo + (self.hi - self.lo) * (spread.height * gaussian(member.seed, 0, MEMBER_HEIGHT_STREAM)).exp();
        }
        member
    }

    /// Time of the first coupled edge, or zero when coupling is disabled.
    pub fn xtalk_phase(&self) -> f64 {
        if self.xtalk == 0.0 {
            0.0
        } else {
            unit(self.seed, 0, XTALK_PHASE_STREAM) / (2.0 * self.xtalk_freq)
        }
    }

    /// The underdamped unit step response, zero before the edge.
    pub fn step(&self, tau: f64) -> f64 {
        if tau <= 0.0 {
            return 0.0;
        }
        let envelope = (-self.zeta * self.wn * tau).exp();
        if envelope == 0.0 {
            return 1.0;
        }
        let damping = (1.0 - self.zeta * self.zeta).sqrt();
        let (sin, cos) = (self.wn * damping * tau).sin_cos();
        1.0 - envelope * (cos + self.zeta / damping * sin)
    }

    /// The noiseless waveform with pulse variation and coupled clock bursts.
    pub fn value(&self, t: f64) -> f64 {
        if t < 0.0 {
            return self.lo;
        }
        let period = self.period();
        let mut cycle = (t / period).floor();
        // Correct division rounding at an exact source breakpoint.
        if (cycle + 1.0) * period <= t {
            cycle += 1.0;
        } else if cycle * period > t {
            cycle -= 1.0;
        }
        let start = cycle * period;
        let high_time = self.duty * period;
        let varied = self.wobble != 0.0 || self.runt != 0.0;
        let mut response = self.step(t - start) - self.step(t - (start + high_time));
        if varied {
            response *= self.pulse_height(cycle as u64);
        }
        if cycle >= 1.0 {
            let previous = (cycle - 1.0) * period;
            // A complete previous pulse is one rising step minus one falling step.
            let mut pulse = self.step(t - previous) - self.step(t - (previous + high_time));
            if varied {
                pulse *= self.pulse_height((cycle - 1.0) as u64);
            }
            response += pulse;
        }
        // Preserve the original arithmetic when pulse variation is disabled.
        let value = if varied {
            self.lo + response
        } else {
            self.lo + (self.hi - self.lo) * response
        };
        if self.xtalk == 0.0 {
            value
        } else {
            value + self.coupled(t)
        }
    }

    fn pulse_height(&self, cycle: u64) -> f64 {
        let wobble = if self.wobble == 0.0 {
            1.0
        } else {
            1.0 + self.wobble * gaussian(self.seed, cycle, HEIGHT_STREAM)
        };
        let runt = if self.runt != 0.0 && unit(self.seed, cycle, RUNT_CHOICE_STREAM) < self.runt {
            0.55 + 0.25 * unit(self.seed, cycle, RUNT_HEIGHT_STREAM)
        } else {
            1.0
        };
        (self.hi - self.lo) * wobble * runt
    }

    fn coupled(&self, time: f64) -> f64 {
        let phase = self.xtalk_phase();
        if time < phase {
            return 0.0;
        }
        let spacing = 1.0 / (2.0 * self.xtalk_freq);
        let mut edge = ((time - phase) / spacing).floor() as u64;
        // Correct division rounding at an exact clock breakpoint.
        if phase + (edge + 1) as f64 * spacing <= time {
            edge += 1;
        } else if phase + edge as f64 * spacing > time {
            edge -= 1;
        }
        let omega = TAU * XTALK_RING;
        let peak_time = (omega * XTALK_DECAY).atan() / omega;
        let peak = (-peak_time / XTALK_DECAY).exp() * (omega * peak_time).sin();
        let mut value = 0.0;
        loop {
            let x = time - (phase + edge as f64 * spacing);
            if x > 12.0 * XTALK_DECAY {
                break;
            }
            let polarity = if edge % 2 == 0 { 1.0 } else { -1.0 };
            value += polarity * self.xtalk * (-x / XTALK_DECAY).exp() * (omega * x).sin() / peak;
            if edge == 0 {
                break;
            }
            edge -= 1;
        }
        value
    }

    /// The theoretical overshoot as a fraction of the step height.
    pub fn overshoot(&self) -> f64 {
        (-PI * self.zeta / (1.0 - self.zeta * self.zeta).sqrt()).exp()
    }

    /// The ringing frequency in Hz.
    pub fn ring_freq(&self) -> f64 {
        self.wn * (1.0 - self.zeta * self.zeta).sqrt() / TAU
    }

    /// Sample with fine edge steps, growing flat steps, and exact breakpoints.
    pub fn record(&self, n: usize) -> (Vec<f64>, Vec<f32>) {
        let mut times = Vec::with_capacity(n);
        let mut values = Vec::with_capacity(n);
        let mut noise = Noise::new(self.seed);
        let period = self.period();
        let coarse = period / 64.0;
        let fine = (1.0 / (24.0 * self.ring_freq())).min(coarse);
        let settling = 6.0 / (self.zeta * self.wn);
        let has_falling_edge = self.duty > 0.0 && self.duty < 1.0;
        let mut cycle = 0usize;
        let mut next_is_rising = !has_falling_edge;
        let mut next_edge = if has_falling_edge { self.duty * period } else { period };
        let mut last_edge = 0.0;
        let coupled = self.xtalk != 0.0;
        let xtalk_fine = fine.min(1.0 / (12.0 * XTALK_RING));
        let xtalk_spacing = if coupled { 1.0 / (2.0 * self.xtalk_freq) } else { f64::INFINITY };
        let xtalk_phase = self.xtalk_phase();
        let mut xtalk_edge = 0u64;
        let mut next_xtalk = xtalk_phase;
        let mut last_xtalk = f64::NEG_INFINITY;
        let mut time = 0.0;
        let mut step = fine;

        for _ in 0..n {
            times.push(time);
            values.push(self.sample(time, &mut noise));
            step = if coupled && time - last_xtalk <= 4.0 * XTALK_DECAY {
                xtalk_fine
            } else if time - last_edge <= settling {
                fine
            } else {
                (step * 1.5).min(coarse)
            };
            let mut next = time + step;
            // Round down so floating-point addition cannot exceed the step bound.
            if next - time > step {
                next = f64::from_bits(next.to_bits() - 1);
            }
            if next <= time {
                next = f64::from_bits(time.to_bits() + 1);
            }
            time = next.min(next_edge);
            if coupled {
                time = time.min(next_xtalk);
            }
            if time == next_edge {
                last_edge = time;
                step = fine;
                if next_is_rising {
                    cycle += 1;
                    if has_falling_edge {
                        next_edge = cycle as f64 * period + self.duty * period;
                        next_is_rising = false;
                    } else {
                        next_edge = (cycle + 1) as f64 * period;
                    }
                } else {
                    next_edge = (cycle + 1) as f64 * period;
                    next_is_rising = true;
                }
            }
            if coupled && time == next_xtalk {
                last_xtalk = time;
                step = xtalk_fine;
                xtalk_edge += 1;
                next_xtalk = xtalk_phase + xtalk_edge as f64 * xtalk_spacing;
            }
        }
        (times, values)
    }

    /// Sample uniformly at the given frequency in Hz.
    pub fn uniform(&self, fs: f64, n: usize) -> Vec<f32> {
        let mut values = Vec::with_capacity(n);
        let mut noise = Noise::new(self.seed);
        for i in 0..n {
            values.push(self.sample(i as f64 / fs, &mut noise));
        }
        values
    }

    fn sample(&self, time: f64, noise: &mut Noise) -> f32 {
        let clean = self.value(time);
        if self.noise == 0.0 {
            clean as f32
        } else {
            (clean + self.noise * noise.gaussian()) as f32
        }
    }
}

fn mix(mut bits: u64) -> u64 {
    bits = bits.wrapping_add(0x9e37_79b9_7f4a_7c15);
    bits = (bits ^ (bits >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    bits = (bits ^ (bits >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    bits ^ (bits >> 31)
}

fn unit(seed: u64, index: u64, stream: u64) -> f64 {
    let key = mix(seed ^ mix(index)).wrapping_add(stream.wrapping_mul(0x9e37_79b9_7f4a_7c15));
    let bits = mix(key) >> 12;
    // Midpoints of 52-bit bins stay strictly between zero and one.
    (bits as f64 + 0.5) / 4_503_599_627_370_496.0
}

fn gaussian(seed: u64, index: u64, stream: u64) -> f64 {
    let radius = (-2.0 * unit(seed, index, stream).ln()).sqrt();
    (radius * (TAU * unit(seed, index, stream + 1)).cos()).clamp(-4.0, 4.0)
}

struct Noise {
    state: u64,
    spare: Option<f64>,
}

impl Noise {
    fn new(seed: u64) -> Self {
        // The all-zero state cannot advance in a xorshift generator.
        Self { state: if seed == 0 { 1 } else { seed }, spare: None }
    }

    fn unit(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let bits = self.state.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 12;
        // Midpoints of 52-bit bins stay strictly between zero and one.
        (bits as f64 + 0.5) / 4_503_599_627_370_496.0
    }

    fn gaussian(&mut self) -> f64 {
        if let Some(value) = self.spare.take() {
            return value;
        }
        let radius = (-2.0 * self.unit().ln()).sqrt();
        let (sin, cos) = (TAU * self.unit()).sin_cos();
        self.spare = Some(radius * sin);
        radius * cos
    }
}

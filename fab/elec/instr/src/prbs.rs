use std::collections::VecDeque;
use std::f64::consts::TAU;

// Each Gaussian reserves two independent streams.
const JITTER_STREAM: u64 = 0;
const NOISE_STREAM: u64 = 2;

/// An NRZ PRBS source through an underdamped channel, with deterministic jitter and noise.
#[derive(Clone, Debug, PartialEq)]
pub struct Prbs {
    /// Bits per second.
    pub bit_rate: f64,
    /// PRBS order: 7, 9, 15, 23 or 31, using x^7+x^6+1, x^9+x^5+1,
    /// x^15+x^14+1, x^23+x^18+1 or x^31+x^28+1 respectively.
    pub order: u8,
    pub lo: f64,
    pub hi: f64,
    /// Channel natural angular frequency in radians per second.
    pub wn: f64,
    /// Damping ratio, strictly between zero and one.
    pub zeta: f64,
    /// RMS transition jitter in seconds, clamped to four standard deviations.
    pub jitter: f64,
    /// RMS voltage noise in volts; zero disables noise.
    pub noise: f64,
    pub seed: u64,
}

impl Prbs {
    /// A 1 Gb/s, 0.8 V PRBS7 source with 0.9 GHz ringing, 6 ps jitter and 4 mV noise.
    pub fn gigabit() -> Self {
        let zeta: f64 = 0.45;
        Self {
            bit_rate: 1e9,
            order: 7,
            lo: 0.0,
            hi: 0.8,
            wn: TAU * 0.9e9 / (1.0 - zeta * zeta).sqrt(),
            zeta,
            jitter: 6e-12,
            noise: 0.004,
            seed: 1,
        }
    }

    /// One unit interval in seconds.
    pub fn ui(&self) -> f64 {
        1.0 / self.bit_rate
    }

    /// Bit k, starting with the all-ones LFSR state and repeating after 2^order - 1 bits.
    pub fn bit(&self, k: u64) -> bool {
        let mut bits = Lfsr::new(self.order);
        for _ in 0..k % bits.period() {
            bits.shift();
        }
        bits.bit()
    }

    /// The jittered boundary before bit k; boundary zero has no jitter.
    pub fn edge_time(&self, k: u64) -> f64 {
        if k == 0 {
            0.0
        } else {
            k as f64 * self.ui() + self.jitter * gaussian(self.seed, k, JITTER_STREAM)
        }
    }

    /// The noiseless waveform, evaluating only the recent, unsettled transitions.
    pub fn value(&self, t: f64) -> f64 {
        if t < 0.0 {
            return self.hi;
        }
        let ui = self.ui();
        let settling = self.settling();
        let margin = 4.0 * self.jitter.abs();
        // All boundaries through start have settled, even with the latest possible jitter.
        // Leave an extra boundary on either side to cover division rounding at breakpoints.
        let start = (((t - settling - margin) / ui).floor() as u64).saturating_sub(1);
        let end = (((t + margin) / ui).ceil() as u64).saturating_add(1);
        let mut bits = Lfsr::new(self.order);
        bits.skip(start);
        let mut value = if bits.bit() { self.hi } else { self.lo };
        for k in start + 1..=end {
            let previous = bits.bit();
            bits.shift();
            if previous != bits.bit() {
                let age = t - self.edge_time(k);
                if age >= 0.0 {
                    let height = if bits.bit() { self.hi - self.lo } else { self.lo - self.hi };
                    value += height * if age > settling { 1.0 } else { self.step(age) };
                }
            }
        }
        value
    }

    /// An independent member with the same channel and signal settings.
    pub fn member(&self, index: u64) -> Prbs {
        Self { seed: self.seed + index + 1, ..self.clone() }
    }

    /// Sample with fine edge steps, growing flat steps, and exact transition breakpoints.
    pub fn record(&self, n: usize) -> (Vec<f64>, Vec<f32>) {
        let mut times = Vec::with_capacity(n);
        let mut values = Vec::with_capacity(n);
        if n == 0 {
            return (times, values);
        }
        let coarse = self.ui() / 8.0;
        let fine = (self.ui() / 32.0).min(1.0 / (24.0 * self.ring_freq()));
        let settling = self.settling();
        let mut edges = Edges::new(self);
        let mut next_edge = edges.next();
        let mut unsettled: VecDeque<Edge> = VecDeque::new();
        let mut settled = self.hi;
        let mut last_edge = 0.0;
        let mut time = 0.0;
        let mut step = fine;

        for index in 0..n {
            while next_edge.time <= time {
                last_edge = next_edge.time;
                unsettled.push_back(next_edge);
                next_edge = edges.next();
                step = fine;
            }
            while unsettled.front().is_some_and(|edge| time - edge.time > settling) {
                settled += unsettled.pop_front().unwrap().height;
            }
            let mut value = settled;
            for edge in &unsettled {
                value += edge.height * self.step(time - edge.time);
            }
            if self.noise != 0.0 {
                value += self.noise * gaussian(self.seed, index as u64, NOISE_STREAM);
            }
            times.push(time);
            values.push(value as f32);
            step = if time - last_edge <= settling { fine } else { (step * 1.5).min(coarse) };
            let mut next = time + step;
            // Round down so floating-point addition cannot exceed the step bound.
            if next - time > step {
                next = f64::from_bits(next.to_bits() - 1);
            }
            if next <= time {
                next = f64::from_bits(time.to_bits() + 1);
            }
            time = next.min(next_edge.time);
        }
        (times, values)
    }

    /// The ringing frequency in Hz.
    pub fn ring_freq(&self) -> f64 {
        self.wn * (1.0 - self.zeta * self.zeta).sqrt() / TAU
    }

    fn settling(&self) -> f64 {
        8.0 / (self.zeta * self.wn)
    }

    fn step(&self, tau: f64) -> f64 {
        if tau <= 0.0 {
            return 0.0;
        }
        let damping = (1.0 - self.zeta * self.zeta).sqrt();
        let (sin, cos) = (self.wn * damping * tau).sin_cos();
        1.0 - (-self.zeta * self.wn * tau).exp() * (cos + self.zeta / damping * sin)
    }
}

struct Lfsr {
    state: u32,
    order: u8,
    tap: u8,
}

impl Lfsr {
    fn new(order: u8) -> Self {
        let tap = match order {
            7 => 6,
            9 => 5,
            15 => 14,
            23 => 18,
            31 => 28,
            _ => panic!("PRBS order must be 7, 9, 15, 23 or 31"),
        };
        Self { state: (1u32 << order) - 1, order, tap }
    }

    fn period(&self) -> u64 {
        (1u64 << self.order) - 1
    }

    fn bit(&self) -> bool {
        self.state & 1 != 0
    }

    fn shift(&mut self) {
        // The low bit is the output: b[k + order] = b[k + tap] XOR b[k].
        let feedback = (self.state ^ (self.state >> self.tap)) & 1;
        self.state = (self.state >> 1) | (feedback << (self.order - 1));
    }

    fn skip(&mut self, k: u64) {
        // Evaluate x^k modulo the recurrence polynomial, then apply it to the state.
        // At most order squarings are needed, independent of the absolute timestamp.
        let mut k = k % self.period();
        let mut power = 2;
        let mut jump = 1;
        while k != 0 {
            if k & 1 != 0 {
                jump = self.product(jump, power);
            }
            power = self.product(power, power);
            k >>= 1;
        }
        let mut state = 0;
        while jump != 0 {
            if jump & 1 != 0 {
                state ^= self.state;
            }
            self.shift();
            jump >>= 1;
        }
        self.state = state;
    }

    fn product(&self, mut a: u32, mut b: u32) -> u32 {
        let top = 1u32 << self.order;
        let polynomial = top | (1u32 << self.tap) | 1;
        let mut product = 0;
        while b != 0 {
            if b & 1 != 0 {
                product ^= a;
            }
            b >>= 1;
            a <<= 1;
            if a & top != 0 {
                a ^= polynomial;
            }
        }
        product
    }
}

struct Edge {
    time: f64,
    height: f64,
}

struct Edges<'a> {
    source: &'a Prbs,
    bits: Lfsr,
    boundary: u64,
    pending: VecDeque<Edge>,
}

impl<'a> Edges<'a> {
    fn new(source: &'a Prbs) -> Self {
        Self { source, bits: Lfsr::new(source.order), boundary: 0, pending: VecDeque::new() }
    }

    fn next(&mut self) -> Edge {
        let margin = 4.0 * self.source.jitter.abs();
        // Look ahead far enough that even reordered jittered edges emerge in time order.
        while self.pending.front().is_none_or(|edge| (self.boundary + 1) as f64 * self.source.ui() - margin <= edge.time) {
            let previous = self.bits.bit();
            self.bits.shift();
            self.boundary += 1;
            if previous != self.bits.bit() {
                let time = self.source.edge_time(self.boundary);
                let height = if self.bits.bit() { self.source.hi - self.source.lo } else { self.source.lo - self.source.hi };
                let at = self.pending.partition_point(|edge| edge.time <= time);
                self.pending.insert(at, Edge { time, height });
            }
        }
        self.pending.pop_front().unwrap()
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

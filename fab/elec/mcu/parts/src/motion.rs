use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

const SEGMENT: usize = 4096;

/// Acceleration in micro-g and angular rate in milli-degrees/s, along the sensor's axes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MotionSample {
    pub accel_ug: [i32; 3],
    pub gyro_mdps: [i32; 3],
}

/// A motion or sound sample in a streamed input.
pub trait TrackSample: Copy + Default + Send + Sync {
    /// The name a live run gives the track when it holds for more samples.
    const KIND: &'static str;
}

impl TrackSample for MotionSample {
    const KIND: &'static str = "motion";
}

impl TrackSample for i16 {
    const KIND: &'static str = "sound";
}

struct Segments<T> {
    full: Vec<Arc<[T]>>,
    current: Arc<[T]>,
    spare: Option<Arc<[T]>>,
}

fn empty_segment<T: TrackSample>() -> Arc<[T]> {
    vec![T::default(); SEGMENT].into()
}

/// An append-only record at a fixed rate on the electronics run clock.
pub struct Track<T> {
    origin_ps: u64,
    pub(super) rate: (u32, u32),
    segments: Mutex<Segments<T>>,
    count: AtomicU64,
    closed: AtomicBool,
    /// Reads that found the open track short of their instant.
    short: AtomicU64,
}

/// Sensor-axis motion, interpolated between samples and held after close.
pub type MotionTrack = Track<MotionSample>;
/// Mono PCM, one signed full-scale sample per period.
pub type SoundTrack = Track<i16>;

/// One track as a part holds it or a stimulus hands it over:
/// compared by identity, so a part's state and a replayed input stay comparable.
#[derive(Clone)]
pub struct TrackRef<T = MotionSample>(pub Arc<Track<T>>);

impl<T> PartialEq for TrackRef<T> {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl<T> Eq for TrackRef<T> {}

impl<T: TrackSample> fmt::Debug for TrackRef<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Track").field("origin_ps", &self.0.origin_ps).field("rate", &self.0.rate).field("len", &self.0.len()).finish()
    }
}

/// The sample at the requested instant, or its position outside the available record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackRead<T = MotionSample> {
    Before,
    Sample(T),
    Ahead,
}

impl<T: TrackSample> Track<T> {
    /// Starts a track at the given picosecond with a nonzero rate in samples per second.
    pub fn new(origin_ps: u64, rate: (u32, u32)) -> Self {
        assert!(rate.0 != 0 && rate.1 != 0, "{} track rate must have nonzero numerator and denominator", T::KIND);
        Self {
            origin_ps,
            rate,
            segments: Mutex::new(Segments {
                full: Vec::with_capacity(16),
                current: empty_segment(),
                spare: Some(empty_segment()),
            }),
            count: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            short: AtomicU64::new(0),
        }
    }

    /// Readies memory for the next segment change. Call it from a thread other than the writer's,
    /// as often as convenient; it does nothing when the spare and the table room are already there.
    pub fn reserve(&self) {
        loop {
            let (need_spare, full_capacity) = {
                let segments = self.segments.lock().expect("track segment table poisoned");
                let capacity = segments.full.capacity();
                (segments.spare.is_none(), (segments.full.len() == capacity).then_some(capacity))
            };
            if !need_spare && full_capacity.is_none() {
                return;
            }

            let mut spare = need_spare.then(empty_segment);
            let mut table = full_capacity.map(|capacity| Vec::with_capacity(16.max(2 * capacity)));
            let mut segments = self.segments.lock().expect("track segment table poisoned");
            if segments.spare.is_none() {
                segments.spare = spare.take();
            }
            if let Some(table) = &mut table {
                if segments.full.len() == segments.full.capacity() && table.capacity() > segments.full.len() {
                    table.append(&mut segments.full);
                    std::mem::swap(&mut segments.full, table);
                }
            }
            if segments.spare.is_some() && segments.full.len() < segments.full.capacity() {
                return;
            }
            // The writer crossed another boundary while we were allocating. Check what it needs now.
        }
    }

    /// Appends the single writer's next sample. Returns true if the writer had to allocate
    /// because `reserve()` had not kept up.
    pub fn push(&self, sample: T) -> bool {
        let count = self.len();
        let offset = (count % SEGMENT as u64) as usize;
        let mut segments = self.segments.lock().expect("track segment table poisoned");
        let current = Arc::get_mut(&mut segments.current).expect("only the writer owns the current segment");
        current[offset] = sample;
        let mut allocated = false;
        if offset + 1 == SEGMENT {
            let next = match segments.spare.take() {
                Some(spare) => spare,
                None => {
                    drop(segments);
                    let next = empty_segment();
                    segments = self.segments.lock().expect("track segment table poisoned");
                    allocated = true;
                    next
                }
            };
            let full = std::mem::replace(&mut segments.current, next);
            allocated |= segments.full.len() == segments.full.capacity();
            segments.full.push(full);
        }
        self.count.store(count + 1, Ordering::Release);
        allocated
    }

    /// Marks the end of the recording and releases the live run's hold.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
    }

    /// Returns the number of samples the writer has published.
    pub fn len(&self) -> u64 {
        self.count.load(Ordering::Acquire)
    }

    /// Reports whether the writer has finished recording.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    /// Seconds between samples.
    pub fn period_s(&self) -> f64 {
        f64::from(self.rate.1) / f64::from(self.rate.0)
    }

    /// The run-clock time (seconds) of the last written sample: a read at or before it is
    /// answered (`at`). One period before the origin while the track is empty. An f64 of the
    /// exact instant, for a live run's hold (`fab_elec_sim::input`), whose margin of one period
    /// absorbs its rounding.
    pub fn horizon_s(&self) -> f64 {
        self.origin_ps as f64 * 1e-12 + (self.len() as f64 - 1.0) * self.period_s()
    }

    /// How many samples put the horizon (`horizon_s`) at or past `t`, seconds: none for an
    /// instant a period or more before the origin.
    pub fn len_to_cover(&self, t: f64) -> u64 {
        let periods = ((t - self.origin_ps as f64 * 1e-12) / self.period_s()).ceil() + 1.0;
        if periods > 0.0 { periods as u64 } else { 0 }
    }

    pub(super) fn count_short(&self) {
        self.short.fetch_add(1, Ordering::Relaxed);
    }

    /// How many reads found the open track short: zero in every correct run.
    pub fn short_reads(&self) -> u64 {
        self.short.load(Ordering::Relaxed)
    }

    /// Returns the sample at index k if the writer has published it.
    pub fn sample(&self, k: u64) -> Option<T> {
        (k < self.len()).then(|| self.written_sample(k))
    }

    fn written_sample(&self, k: u64) -> T {
        let index = usize::try_from(k / SEGMENT as u64).expect("written segment fits in the table");
        let offset = (k % SEGMENT as u64) as usize;
        let full = {
            let segments = self.segments.lock().expect("track segment table poisoned");
            match segments.full.get(index) {
                Some(full) => Arc::clone(full),
                None => return segments.current[offset],
            }
        };
        full[offset]
    }

    // Sample index and fraction of a period at a run-clock instant. For a board tick
    // between picoseconds, carry the fractional picosecond times the rate numerator.
    fn position(&self, t_ps: u128, fraction: u128) -> Option<(u128, u128, u128)> {
        let delta = t_ps.checked_sub(u128::from(self.origin_ps))?;
        let num = u128::from(self.rate.0);
        let p = u128::from(self.rate.1) * 1_000_000_000_000;
        let h = delta / p;
        let l = delta % p;
        let m = l * num + fraction;
        Some((h * num + m / p, m % p, p))
    }
}

impl MotionTrack {
    /// Counts a short read and returns the last written motion, if any, for the sensor to hold.
    pub fn read_short(&self) -> Option<MotionSample> {
        self.count_short();
        self.len().checked_sub(1).map(|last| self.written_sample(last))
    }

    /// Interpolates motion at a run-clock instant in picoseconds.
    pub fn at(&self, t_ps: u128) -> TrackRead {
        let Some((k, r, p)) = self.position(t_ps, 0) else { return TrackRead::Before };
        let closed = self.is_closed();
        let Some(last) = self.len().checked_sub(1) else {
            return if closed { TrackRead::Before } else { TrackRead::Ahead };
        };
        if k > u128::from(last) || (k == u128::from(last) && r != 0) {
            return if closed { TrackRead::Sample(self.written_sample(last)) } else { TrackRead::Ahead };
        }
        let k = k as u64;
        let a = self.written_sample(k);
        if r == 0 {
            return TrackRead::Sample(a);
        }
        let b = self.written_sample(k + 1);
        let interpolate = |a: i32, b: i32| {
            let product = (i128::from(b) - i128::from(a)) * r as i128;
            let step = product.signum() * ((product.abs() + p as i128 / 2) / p as i128);
            (i128::from(a) + step) as i32
        };
        TrackRead::Sample(MotionSample {
            accel_ug: std::array::from_fn(|i| interpolate(a.accel_ug[i], b.accel_ug[i])),
            gyro_mdps: std::array::from_fn(|i| interpolate(a.gyro_mdps[i], b.gyro_mdps[i])),
        })
    }
}

impl SoundTrack {
    /// Appends as much of the single writer's buffer as fits in the current segment.
    /// At a run loop top a busy reader leaves the whole buffer for the next check.
    pub fn try_append(&self, samples: &[i16]) -> usize {
        let mut segments = match self.segments.try_lock() {
            Ok(segments) => segments,
            Err(std::sync::TryLockError::WouldBlock) => return 0,
            Err(std::sync::TryLockError::Poisoned(_)) => panic!("track segment table poisoned"),
        };
        let count = self.len();
        let offset = (count % SEGMENT as u64) as usize;
        let n = samples.len().min(SEGMENT - offset);
        let current = Arc::get_mut(&mut segments.current).expect("only the writer owns the current segment");
        current[offset..offset + n].copy_from_slice(&samples[..n]);
        if offset + n == SEGMENT {
            let next = segments.spare.take().unwrap_or_else(empty_segment);
            let full = std::mem::replace(&mut segments.current, next);
            segments.full.push(full);
        }
        self.count.store(count + n as u64, Ordering::Release);
        n
    }

    /// A playback of the closed recording starting at another run instant. Completed segments
    /// are shared with earlier plays. Only the final, writable segment needs its own copy.
    pub fn replay_at(&self, origin_ps: u64) -> Self {
        assert!(self.is_closed(), "only a closed sound recording can be replayed");
        let segments = self.segments.lock().expect("track segment table poisoned");
        Self {
            origin_ps,
            rate: self.rate,
            segments: Mutex::new(Segments {
                full: segments.full.clone(), current: Arc::from(&segments.current[..]), spare: None,
            }),
            count: AtomicU64::new(self.len()),
            closed: AtomicBool::new(true),
            short: AtomicU64::new(0),
        }
    }

    /// The PCM sample whose period contains the frame's first edge. Carry the tick's
    /// sub-picosecond remainder so an edge exactly on a sample boundary reads that sample.
    pub(super) fn at_tick(&self, tick: u64, tick_hz: u64) -> TrackRead<i16> {
        let time = u128::from(tick) * 1_000_000_000_000;
        let hz = u128::from(tick_hz);
        let fraction = time % hz * u128::from(self.rate.0) / hz;
        let Some((k, _, _)) = self.position(time / hz, fraction) else { return TrackRead::Before };
        let closed = self.is_closed();
        if k < u128::from(self.len()) {
            TrackRead::Sample(self.written_sample(k as u64))
        } else if closed {
            TrackRead::Sample(0)
        } else {
            TrackRead::Ahead
        }
    }
}

//! The Listen tap and the app's player, for tests that hear a host's run as a person hears it:
//! shared by the host's tests and the firmware's (`#[path]`).

use fab_elec_sim::listen::{AudioTap, ListenBus, Player, RecordSpec, TapMix, TapSpec, TapVoice};
use fab_elec_sim::recording::{settle_tapes, Chunk, Delivery, Shape};
use fab_elec_sim::stamp::UnknownInfo;
use fab_elec_sim::Sink;
use fab_elec_simhost::SimHost;
use fab_elec_view::live::sim::{SimAction, SimLive, SinkSlot};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The most frames a phase takes.
const FRAMES: u32 = 50_000;

/// The device rate the Listen rig plays at, the voice's gain, and its tap's slot.
pub const EAR_RATE: f64 = 48_000.0;
pub const EAR_GAIN: f32 = 0.1;
const EAR_SLOT: u64 = 1;

/// The Listen tap as the app installs it (`listen::start`), and the app's player on the test's
/// thread playing what the wall clock owes a 48 kHz device at each frame, as the app's silent
/// clock plays; the run paced to [`PACE`] of real time at most ([`Pace`]), as a board too heavy
/// to keep up runs, so the listener hears it as its recording, and the host is told so at each
/// frame, as the app's frame tells it (`listen::frame`). The tap records one net at
/// [`EAR_GAIN`] with no DC block: the take's samples are the net's volts times the gain.
pub struct Ears {
    pub bus: Arc<ListenBus>,
    player: Player,
    out: Vec<f32>,
    owed: f64,
    at: Instant,
    pub tee: Option<Tee>,
}

/// The chunks a tapped Listen sent ([`Ears::listen`]), in order, each as it went on to the
/// player: its take's number, index and run time, and its samples.
pub struct Tee {
    rx: Receiver<Delivery>,
    pub got: Vec<(u32, u64, f64, Chunk)>,
}

impl Ears {
    pub fn new(host: &mut SimHost, live: &mut SimLive, net: &str, window_s: f64) -> Ears {
        let voice = TapVoice { net: net.into(), reference: None, gain: EAR_GAIN };
        let ears = Self::listen(host, live, voice, 0.0, window_s, false);
        pace(host);
        ears
    }

    /// A Listen of `voice` with a DC block at `dc_block_hz` (as the app's Scope lane makes it,
    /// `listen::sync`), unpaced; with `tee`, its chunks are seen on their way to the player
    /// ([`Ears::tee`]).
    pub fn listen(host: &mut SimHost, live: &mut SimLive, voice: TapVoice, dc_block_hz: f64, window_s: f64, tee: bool) -> Ears {
        let bus = live.listen_bus();
        bus.ring.set_device_rate(EAR_RATE as u32);
        bus.ring.set_open(true);
        let links = bus.take_player().expect("the views' bus has its player's ends");
        let mix = TapMix::new(1);
        // the player takes the recording's shape before the tap's first chunk can reach it
        bus.links.shapes.try_send(Box::new(Shape::new(EAR_SLOT, window_s, 1, mix.clone()))).ok().expect("the player takes the shape");
        let record = bus.record(EAR_SLOT, EAR_SLOT, window_s);
        let (record, tee) = if tee {
            let (tx, rx) = sync_channel(4096);
            (RecordSpec { chunks: tx, ..record }, Some(Tee { rx, got: Vec::new() }))
        } else {
            (record, None)
        };
        let tap = AudioTap::new(TapSpec { voices: vec![voice], latency: 0.06, dc_block_hz, mix }, Arc::clone(&bus.ring)).recording(record);
        host.act(&SimAction::Listen { id: EAR_SLOT, sink: SinkSlot::new(Box::new(tap)) });
        Ears { bus, player: Player::new(Some(links)), out: vec![0.0; 4096], owed: 0.0, at: Instant::now(), tee }
    }

    /// One app frame: the device plays what the wall clock owes it (a stall made up to one
    /// buffer at most), the host hears whether the recording plays, and steps. Returns what the
    /// device played.
    pub fn frame(&mut self, host: &mut SimHost, live: &mut SimLive, start: Instant) -> &[f32] {
        let now = Instant::now();
        self.owed = (self.owed + (now - self.at).as_secs_f64() * EAR_RATE).min(self.out.len() as f64);
        self.at = now;
        let n = self.owed as usize;
        self.owed -= n as f64;
        // a tapped Listen's chunks, seen and passed on to the player
        if let Some(tee) = &mut self.tee {
            while let Ok(d) = tee.rx.try_recv() {
                tee.got.push((d.take.take, d.index, d.t, d.chunk.clone()));
                assert!(self.bus.links.chunks.try_send(d).is_ok(), "the player's queue takes each tapped chunk");
            }
        }
        // Stop and Pause silence what is heard, as the app's frame says (`listen::frame`)
        self.bus.ring.set_hushed(!live.playing);
        self.player.fill(&self.bus.ring, &mut self.out[..n], EAR_RATE);
        self.bus.links.spent.drain();
        host.set_rendering(self.bus.ring.plays_recording());
        host.step(start.elapsed().as_secs_f64(), live);
        &self.out[..n]
    }

    /// Frames at 2 ms until `done` holds, bounded by frames, not wall time: a busy box makes each
    /// frame wait longer, and only a run that stopped making progress runs out of them. A host
    /// with no run and none coming (`SimHost::awaits_commit`) fails at once.
    pub fn until(&mut self, host: &mut SimHost, live: &mut SimLive, start: Instant, what: &str, mut done: impl FnMut(&SimHost, &Ears) -> bool) {
        for _ in 0..FRAMES {
            self.frame(host, live, start);
            assert!(host.error().is_none(), "{what}: the run failed: {:?}", host.error());
            assert!(!host.awaits_commit(), "{what}: no run, and none starts until a design is committed (the app's frame commits every frame; a test that steps the host commits after a Play that follows a Stop)");
            if done(host, self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let tape = self.bus.tapes.current(EAR_SLOT).map(|tape| (tape.frames(), tape.rate));
        panic!("{what}: not reached within {FRAMES} frames; the run at {:?} s, heard at {:?} s, {} rewinds, the take's (frames, rate) {tape:?}, {:?}",
            host.values().map(|v| v.t), self.heard_at(), host.store().rewinds, self.bus.ring.play_status());
    }

    /// The run's time the listener hears now, while the recording plays.
    pub fn heard_at(&self) -> Option<f64> {
        self.bus.ring.heard_at()
    }

    /// The take over `[t0, t1)` of the run's time once its tape holds it.
    pub fn take(&self, t0: f64, t1: f64) -> Option<Take> {
        settle_tapes();
        let tape = self.bus.tapes.current(EAR_SLOT)?;
        let (a, b) = ((t0 * tape.rate) as u64, (t1 * tape.rate).ceil() as u64);
        if tape.frames() < b {
            return None;
        }
        let mut out = vec![0.0f32; (b - a) as usize * tape.voices];
        let n = tape.read(a, &mut out).expect("the take's tape reads");
        let samples = out.into_iter().step_by(tape.voices).collect();
        (n as u64 == b - a).then_some(Take { samples, rate: tape.rate, first: a })
    }

    /// Frames until the run went back for an edit at `t` (the store had `rewinds` before it) and
    /// rendered on past it: the take over `SIDE` on each side of `t`, as recorded again.
    pub fn around(&mut self, host: &mut SimHost, live: &mut SimLive, start: Instant, t: f64, rewinds: u64, what: &str) -> Take {
        let mut take = None;
        self.until(host, live, start, what, |h, e| {
            h.store().rewinds > rewinds && h.values().is_some_and(|v| v.t > t + SIDE) && {
                take = e.take(t - SIDE, t + SIDE);
                take.is_some()
            }
        });
        take.expect("the take around the edit")
    }
}

/// A stretch of a take: its samples (one voice) at the take's rate from frame `first`. The take
/// starts with the run: frame k is the period from k / rate of the run's time.
pub struct Take {
    pub samples: Vec<f32>,
    pub rate: f64,
    pub first: u64,
}

impl Take {
    /// The samples over `[t0, t1)` of the run's time.
    pub fn between(&self, t0: f64, t1: f64) -> &[f32] {
        let at = |t: f64| ((t * self.rate) as u64).saturating_sub(self.first).min(self.samples.len() as u64) as usize;
        &self.samples[at(t0)..at(t1)]
    }
}

/// The rig's recording window, seconds.
pub const WINDOW: f64 = 0.1;
/// The share of real time a run on the rig makes at most ([`Pace`]).
const PACE: f64 = 0.25;
/// The slot of the rig's pacing sink.
const PACE_SLOT: u64 = 2;
/// How far the run is ahead of the play head when an edit is made: an edit taken at the run's
/// front instead would land past the stretch checked after it.
const AHEAD: f64 = 0.04;
/// The stretch of the take checked on each side of an edit.
pub const SIDE: f64 = 0.025;

/// Ready for an edit: the play head past `last` and the stretch checked after it, and the run
/// well ahead of the play head ([`AHEAD`]).
pub fn ready(last: f64) -> impl Fn(&SimHost, &Ears) -> bool + Copy {
    move |h, e| e.heard_at().zip(h.values()).is_some_and(|(at, v)| at > last + SIDE && v.t > at + AHEAD)
}

/// Paces `host`'s runs to [`PACE`] of real time at most ([`Pace`]), as a board too heavy to keep up
/// runs: a listener hears the recording on any box, however fast its solver.
pub fn pace(host: &mut SimHost) {
    host.act(&SimAction::ShowScope { id: PACE_SLOT, sink: SinkSlot::new(Box::new(Pace { anchor: None })), column: 0.0 });
}

/// Paces the run it sinks to [`PACE`] of real time at most, as a board too heavy to keep up
/// runs: in its points it sleeps while the run is ahead of the pace since its anchor (the first
/// point, and again after a rest, a rewind, or a wait that left the run behind it, so it never
/// races to catch up). The Listen tap measures the run's speed over the wall time between its
/// points, these sleeps in it, as it measures a slow solver's steps.
struct Pace {
    anchor: Option<(Instant, f64)>,
}

impl Sink for Pace {
    fn begin(&mut self, _unknowns: &[UnknownInfo]) {
        self.anchor = None;
    }

    fn point(&mut self, at: f64, _outer: Option<f64>, _x: &[f64]) {
        let now = Instant::now();
        let (wall, t) = *self.anchor.get_or_insert((now, at));
        let due = wall + Duration::from_secs_f64(((at - t) / PACE).max(0.0));
        if due > now + Duration::from_millis(1) {
            std::thread::sleep(due - now);
        } else if now > due + Duration::from_millis(1) {
            self.anchor = Some((now, at));
        }
    }

    fn idle(&mut self) {
        self.anchor = None;
    }

    fn steady(&mut self, _at: f64, _x: &[f64]) {
        self.anchor = None;
    }
}

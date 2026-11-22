//! The motion link (Q11): a board printed on the plate is a body the Mechanics sim moves, and its
//! IMU reads that motion. While Mechanics runs a sim with the board and the board's bench plays,
//! the bench's run leads the sim ([`Leader`]): each step the sim may take is one sample of a
//! motion track the run holds for ([`TrackInput`]), probed at the sensor on the board's body
//! ([`motion_mounts`], [`MechSim::probe_on`]) and written on the sim thread in the IMU's units.
//!
//! - A link starts at the run's time as the host sees it (`SimHost::input_time`): its tracks
//!   start there, the run takes their sources first (`SimHost::set_inputs`), and the IMU takes
//!   its track as a timed input (`BoardInput::Track`) only once the run is past the first instant
//!   it asked the track to cover (`TrackInput::first_want`). Every step from there on was held
//!   for the track, so the IMU never reads past what the sim wrote (`fab_elec_sim::input`).
//! - A new sim (a rebuilt world), a new run, a pause, or the board or Mechanics gone ends the
//!   link: its tracks close (the IMU keeps their last sample, the run holds for them no more) and
//!   the sim goes back to its own clock. The next link is new tracks from the run's time then.
//! - Opening Mechanics with such a board plays its bench, once a showing, as the closed box does.
//! - The Mechanics views draw the parts at the instant the bench's views show
//!   ([`MotionLink::instant`]): the led sim records each step's poses (`PoseRecord`), so the
//!   board's screen and the body it is on show one instant, about the lead behind the sim's newest
//!   step, and a review of the take shows the parts as they were at its instant.
//!
//! Cost: per frame, a few compares, one `MotionTrack::reserve` a track (on the UI thread, so
//! the sim thread's push does not allocate while it keeps up) and one step's poses copied from the
//! record (O(parts), with the snapshot they are drawn into); the mounts are found again only when
//! the plate or the design changes (O(placed parts + objects)). On the sim thread, per step: the
//! probes' sampling, one push a track and the parts' poses recorded (56 bytes a part).

use crate::elec_host::ElecHost;
use crate::mechanics::MechSim;
use fab::api::{AppState, MechLead, PlateMode};
use fab_elec_bench::{BoardInput, SimCmd};
use fab_elec_core::CompRef;
use fab_elec_design::Design;
use fab_elec_fw::TrackInput;
use fab_elec_mcu_parts::{MotionSample, MotionTrack, TrackRef};
use fab_elec_mech::{motion_mounts, MotionMount};
use fab_elec_sim::input::{InputSource, Inputs};
use fab_elec_simhost::Limit;
use fab_elec_view::live::sim::SimAction;
use fab_model::eval::Evaluated;
use makepad_csg_mech::runner::{Lead, Leader, PoseRecord, ProbeSample, Probes, HZ};
use makepad_widgets::*;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Weak};

/// Standard gravity, mm/s²: a micro-g's millionth part.
const G_MM: f64 = 9806.65;

/// What a link's pose record keeps at full resolution (past it its older steps thin): over three
/// minutes of a dozen parts at the sim's 240 Hz.
const POSE_BYTES: usize = 32 << 20;

/// A probe's reading in the IMU's units: micro-g and milli-degrees per second, rounded.
fn imu_sample(s: &ProbeSample) -> MotionSample {
    MotionSample {
        accel_ug: s.specific_force.map(|a| (a / G_MM * 1e6).round() as i32),
        gyro_mdps: s.rate.map(|w| (w.to_degrees() * 1e3).round() as i32),
    }
}

/// The parts' poses the links of one run in one sim recorded, for the instant the views draw.
struct Record {
    /// The Mechanics sim's number and the bench run's.
    sim: u64,
    run: u64,
    /// The first link's origin, picoseconds of the run: the record's step 1 is the sim's state then.
    origin_ps: u64,
    poses: Arc<PoseRecord>,
}

impl Record {
    /// The record's step whose pose is the sim's state at `t`, or the last before it (seconds of
    /// the run): the pose after a lead's step k is the sim's state at its tracks' sample k - 1.
    fn step(&self, t: f64) -> u64 {
        ((t - self.origin_ps as f64 * 1e-12) * HZ).floor().max(0.0) as u64 + 1
    }
}

/// One link of a sim and a run.
struct Link {
    record: Record,
    /// Each probed sensor and the track the run holds for.
    tracks: Vec<(CompRef, Arc<TrackInput>)>,
    /// The IMUs took their tracks.
    attached: bool,
    leader: Leader,
}

impl Link {
    /// The tracks close and the sim goes back to its own clock; what it recorded stays for a review.
    fn end(self) -> Record {
        for (_, input) in &self.tracks {
            input.track.close();
        }
        self.leader.set(None);
        self.record
    }
}

#[derive(Default)]
pub struct MotionLink {
    /// The plate and design the mounts are of, and the mounts.
    of: Option<(Weak<Evaluated>, Weak<Design>)>,
    mounts: Vec<MotionMount>,
    link: Option<Link>,
    /// The poses the last link recorded, while the same run's take may be reviewed in the same sim.
    reviewed: Option<Record>,
    /// This showing of Mechanics played the board's bench.
    played: bool,
}

impl MotionLink {
    /// Mechanics shows a plate with a printed board: the electronics host runs, for its design.
    pub fn wants_elec(state: &AppState, plate: Option<&Arc<Evaluated>>) -> bool {
        state.ui.in_mode(PlateMode::Mechanics)
            && plate.is_some_and(|ev| ev.plate.parts.iter().any(|p| p.link.as_deref().is_some_and(|l| l.starts_with("board:"))))
    }

    /// The step of a pose record the Mechanics views draw (`MechSim::show_instant`), at the
    /// instant the bench's views show, so the board's screen and the body it is on show one
    /// instant:
    /// - while a link leads the sim, the link's record at the run's newest values, the instant the
    ///   board's screen shows (about the lead, two of the bench's publications, behind the sim's
    ///   newest step; also while the take plays, whose screen frame is the newest too), or at a
    ///   review's instant (`SimHost::review_instant`); the record keeps from the take's start, so
    ///   a review finds the poses of its instant;
    /// - after the link ended (a pause), its record while the same run's take is reviewed in the
    ///   same sim; the sim has gone on by its own clock, and the newest pose shows again once the
    ///   review ends. The next link of that run and sim goes on in the same record.
    ///
    /// None otherwise. O(1), once a frame before the Mechanics sim's own.
    pub fn instant(&mut self, mech: &MechSim, elec: Option<&mut ElecHost>) -> Option<(Arc<PoseRecord>, u64)> {
        let sim = elec.and_then(|elec| elec.sim_mut());
        let (run, sim_no) = (sim.as_ref().and_then(|sim| sim.live_run()), mech.runner().map(|(_, n)| n));
        self.reviewed.take_if(|r| Some(r.run) != run || Some(r.sim) != sim_no);
        let sim = sim?;
        let review = sim.review_instant();
        let (record, shown) = match (&self.link, &self.reviewed, review) {
            (Some(link), _, _) => {
                let shown = review.unwrap_or_else(|| sim.values().map_or(0.0, |v| v.t));
                let from = sim.take().span().map_or(shown, |(start, _)| start.min(shown));
                link.record.poses.keep_from(link.record.step(from));
                (&link.record, shown)
            }
            (None, Some(reviewed), Some(t)) => (reviewed, t),
            _ => return None,
        };
        Some((record.poses.clone(), record.step(shown)))
    }

    /// Once a frame, after the Mechanics sim's own. While a link leads the sim, Mechanics' bar shows
    /// the lead, the pace the bench's run reaches and whether the run waits for this sim's motion
    /// (`SimHost::status`, O(publications in the rate's window)); the bar redraws when its text
    /// changes.
    pub fn handle_event(&mut self, cx: &mut Cx, state: &mut AppState, plate: Option<&Arc<Evaluated>>, mech: &MechSim, mut elec: Option<&mut ElecHost>) {
        self.follow(cx, state, plate, mech, elec.as_deref_mut());
        state.mech.lead = self.link.as_ref().map(|_| {
            let status = elec.and_then(|elec| elec.sim_mut()).map(|sim| sim.status());
            MechLead {
                rate: status.as_ref().and_then(|s| s.rate_achieved),
                waits_for_motion: status.is_some_and(|s| s.playing && s.limited_by == Some(Limit::Input)),
            }
        });
    }

    fn follow(&mut self, cx: &mut Cx, state: &AppState, plate: Option<&Arc<Evaluated>>, mech: &MechSim, elec: Option<&mut ElecHost>) {
        let (Some(ev), Some(elec)) = (plate.filter(|_| state.ui.in_mode(PlateMode::Mechanics)), elec) else {
            self.played = false;
            self.end();
            return;
        };
        let Some(design) = elec.design().cloned() else { return self.end() };
        self.find_mounts(ev, &design);
        if self.mounts.is_empty() {
            return self.end();
        }
        if !self.played {
            self.played = true;
            cx.action(SimAction::Cmd(SimCmd::Play));
        }
        let (Some(sim), Some((runner, sim_no))) = (elec.sim_mut(), mech.runner()) else { return self.end() };
        let Some(run) = sim.live_run().filter(|_| sim.playing()) else { return self.end() };
        if self.link.as_ref().is_some_and(|l| l.record.sim != sim_no || l.record.run != run) {
            self.end();
        }
        if self.link.is_none() {
            let origin_ps = (sim.input_time().max(0.0) * 1e12) as u64;
            let mut probes = Vec::new();
            let mut tracks = Vec::new();
            for mount in &self.mounts {
                let Some(probe) = mech.probe_on(ev, mount.part, mount.at, mount.axes) else { continue };
                let refdes = design.unit(mount.comp.board).and_then(|u| u.circuit.comps.get(mount.comp.comp.ix())).map_or_else(String::new, |c| c.refdes.clone());
                let track = Arc::new(MotionTrack::new(origin_ps, (HZ as u32, 1)));
                track.reserve();
                probes.push(probe);
                tracks.push((mount.comp, Arc::new(TrackInput::new(track, refdes))));
            }
            let Some((_, first)) = tracks.first() else { return };
            // every track has the one origin and rate, so the run asks each for as many samples
            let allowed = first.allowed();
            let sources: Vec<Arc<dyn InputSource>> = tracks.iter().map(|(_, t)| t.clone() as Arc<dyn InputSource>).collect();
            if !sim.set_inputs(Inputs { sources, want: Arc::new(AtomicU64::new(0)) }) {
                return;
            }
            let written: Vec<Arc<MotionTrack>> = tracks.iter().map(|(_, t)| t.track.clone()).collect();
            let sink = Box::new(move |_step: u64, batch: &[ProbeSample]| {
                for (track, sample) in written.iter().zip(batch) {
                    track.push(imu_sample(sample));
                }
            });
            // the same run in the same sim goes on in the record the last link left, numbered from
            // its first link's origin, so a review across a pause finds both stretches
            let record = match self.reviewed.take().filter(|r| r.run == run && r.sim == sim_no) {
                Some(r) => {
                    r.poses.number_from((origin_ps.saturating_sub(r.origin_ps) as f64 * 1e-12 * HZ).round() as u64 + 1);
                    r
                }
                None => Record { sim: sim_no, run, origin_ps, poses: Arc::new(PoseRecord::new(POSE_BYTES)) },
            };
            let leader = runner.leader();
            leader.set(Some(Lead { allowed, probes: Probes { probes, sink }, poses: Some(record.poses.clone()) }));
            self.link = Some(Link { record, tracks, attached: false, leader });
        }
        let Some(link) = self.link.as_mut() else { return };
        for (_, input) in &link.tracks {
            input.track.reserve();
        }
        let front = sim.values().map_or(0.0, |v| v.t);
        let asked = link.tracks.iter().map(|(_, t)| t.first_want()).try_fold(f64::MIN, |a, w| Some(a.max(w?)));
        if !link.attached && asked.is_some_and(|want| front > want) {
            link.attached = true;
            for (comp, input) in &link.tracks {
                if !sim.command(&SimCmd::Board(BoardInput::Track { comp: *comp, track: TrackRef(input.track.clone()) })) {
                    log!("motion link: the bench's run has no IMU {} to read its track", input.refdes);
                }
            }
        }
    }

    /// The mounts of the plate and design as they are now (found again when either changes).
    fn find_mounts(&mut self, ev: &Arc<Evaluated>, design: &Arc<Design>) {
        if self.of.as_ref().is_some_and(|(e, d)| Weak::as_ptr(e) == Arc::as_ptr(ev) && Weak::as_ptr(d) == Arc::as_ptr(design)) {
            return;
        }
        self.of = Some((Arc::downgrade(ev), Arc::downgrade(design)));
        self.mounts = motion_mounts(design, ev, &mut |why| log!("motion link: {why}"));
    }

    fn end(&mut self) {
        if let Some(link) = self.link.take() {
            self.reviewed = Some(link.end());
        }
    }
}

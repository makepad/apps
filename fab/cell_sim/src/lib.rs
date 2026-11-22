//! The virtual cell: one or two arms and a positioner, the process and the sensors, simulated
//! at a fixed 1 kHz behind the same [`CellLink`] a real cell implements.
//!
//! What the cell is made of is the truth: the nominal models of the [`CellSetup`] with hidden
//! [`Errors`] drawn from a seed (the joints softer than modelled, mass on the tool nobody told
//! the planner of, beads larger than the model expects). The planner gets the nominal models in
//! [`CellInfo`] and sees the truth only as observations: encoders, a tracker on each tool, an
//! IMU on each tool, the melt pool's width, line scans, the positioner's encoders; each with
//! its rate, latency and noise. The same seed gives the same bits.
//!
//! Each arm's controller is reproduced as its preset describes: our impedance setpoints held
//! between arrivals, or the maker's position loop with its own gravity model; setpoints faster
//! than the controller takes are dropped, a lost stream or a large following error stops the
//! arm. Laser-wire deposits beads into the true field at the true TCP (mass balance); a laser
//! cut takes out the kerf along the true beam and the offcut beside it; the part warms with the
//! lasers and warps with its temperature ([`Thermal`]: the true field holds the part cold, the
//! processes and the scanner meet it where it stands hot).

use fab_robot::arm::*;
use fab_robot::cell::{CellSetup, Positioner, Substrate};
use fab_robot::dynamics::*;
use fab_robot::field::*;
use fab_robot::io::*;
use fab_robot::kin::{frames, Frames};
use fab_robot::math::*;
use fab_robot::process::{BeadModel, CutModel, Heat, Thermal};
use fab_robot::safety::{Interlocks, Mode, StopKind, Watchdog};
use std::collections::VecDeque;

#[cfg(test)]
mod tests;

/// The plant's step (s).
pub const DT: f64 = 1e-3;

/// How the cell differs from its nominal models.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Errors {
    /// True joint stiffness over the nominal (every joint).
    pub stiffness: f64,
    /// Mass on the tool the models leave out (kg).
    pub payload: f64,
    /// True friction over the nominal.
    pub friction: f64,
    /// Bead section and width over the model's.
    pub bead_area: f64,
    pub bead_width: f64,
    /// Bead width noise (one sigma, relative) and its correlation time (s).
    pub width_noise: f64,
    pub noise_time: f64,
    /// An extruder's true melt flow over the commanded (filament diameter, slip), and its
    /// true melt lag over the modelled.
    pub flow: f64,
    pub melt_lag: f64,
    /// The heat the part truly takes over what the thermal model says (absorptivity is the
    /// least sure figure in it).
    pub absorb: f64,
}

impl Errors {
    pub const NONE: Errors = Errors { stiffness: 1.0, payload: 0.0, friction: 1.0, bead_area: 1.0, bead_width: 1.0, width_noise: 0.0, noise_time: 0.05, flow: 1.0, melt_lag: 1.0, absorb: 1.0 };
    /// Mis-modelled the way real cells are: joints 40 % softer, 0.3 kg unmodelled on the tool,
    /// friction 30 % up, beads 8 % fuller and 5 % wider with 3 % noise; an extruder 3 % short
    /// of its commanded flow with its melt 30 % slower than modelled; the part taking 20 % more
    /// of the lasers' heat than the model says.
    pub const TYPICAL: Errors = Errors { stiffness: 0.6, payload: 0.3, friction: 1.3, bead_area: 1.08, bead_width: 1.05, width_noise: 0.03, noise_time: 0.05, flow: 0.97, melt_lag: 1.3, absorb: 1.2 };
}

/// One sensor's rate, latency and noise (one sigma, in its unit).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SensorSpec {
    pub hz: f64,
    pub latency: f64,
    pub sigma: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sensors {
    /// Motor encoders (rad).
    pub encoders: SensorSpec,
    /// The tracker on each tool (mm), and its rotation noise (rad).
    pub tracker: SensorSpec,
    pub tracker_rot: f64,
    /// The IMU on each tool (mm/s²), and its gyro noise (rad/s).
    pub imu: SensorSpec,
    pub gyro: f64,
    /// The line scanner (mm) and the points across its line.
    pub scanner: SensorSpec,
    pub scan_points: usize,
    /// The melt pool camera's bead width (mm).
    pub melt_pool: SensorSpec,
    /// The positioner's encoders (rad).
    pub positioner: SensorSpec,
    /// A thermocouple on the plate (K), in a cell with a laser.
    pub thermocouple: SensorSpec,
}

impl Sensors {
    /// Figures for a cell built from good industrial parts (estimates).
    pub const DEFAULT: Sensors = Sensors {
        encoders: SensorSpec { hz: 1000.0, latency: 0.001, sigma: 2e-5 },
        tracker: SensorSpec { hz: 250.0, latency: 0.004, sigma: 0.01 },
        tracker_rot: 5e-5,
        imu: SensorSpec { hz: 1000.0, latency: 0.001, sigma: 30.0 },
        gyro: 5e-4,
        scanner: SensorSpec { hz: 100.0, latency: 0.01, sigma: 0.01 },
        scan_points: 48,
        melt_pool: SensorSpec { hz: 30.0, latency: 0.02, sigma: 0.05 },
        positioner: SensorSpec { hz: 250.0, latency: 0.002, sigma: 5e-6 },
        thermocouple: SensorSpec { hz: 2.0, latency: 0.5, sigma: 0.5 },
    };
}

#[derive(Clone, Debug)]
pub struct SimConfig {
    pub seed: u64,
    pub errors: Errors,
    pub sensors: Sensors,
    pub bead: BeadModel,
    pub cut: CutModel,
    pub thermal: Thermal,
    /// The true field's sample spacing (mm).
    pub field_cell: f64,
    /// Each arm's joint pose at the start (the motors hold it).
    pub home: Vec<Q>,
}

impl SimConfig {
    pub fn new(seed: u64, errors: Errors, home: Vec<Q>) -> SimConfig {
        SimConfig { seed, errors, sensors: Sensors::DEFAULT, bead: BeadModel::NOMINAL, cut: CutModel::NOMINAL, thermal: Thermal::NOMINAL, field_cell: 0.25, home }
    }
}

/// What happened in the process, for the panel, the stats and the tests.
#[derive(Clone, Debug, Default)]
pub struct SimLog {
    /// Beads laid (part frame) and when.
    pub beads: Vec<(f64, Bead)>,
    /// Cut steps (part frame: where the beam focused, its axis) and when.
    pub cuts: Vec<(f64, V3, V3)>,
    /// Bead steps laid with the wire more than its reach above the surface (no fusion), or a
    /// plastic strand pushed out from higher than its own thickness.
    pub lack_of_fusion: u32,
    /// Plastic pieces laid while the extruder's drive was not pushing: the melt still running
    /// out after a stop (ooze, strings across travels).
    pub strings: u32,
    /// Cut steps whose beam reached the substrate within its damaging range.
    pub plate_hits: u32,
    /// Length cut (mm), and the recast and dross the cut faces carry (mm, length-weighted sums).
    pub cut_length: f64,
    pub recast: f64,
    pub dross: f64,
    /// Setpoints the controllers dropped (sent faster than they take).
    pub dropped: u32,
    pub faults: Vec<(f64, ArmId, DriveFault)>,
    /// Steps a drive spent at its torque limit.
    pub saturated: u64,
    /// Deposited volume (mm³).
    pub deposited: f64,
}

#[derive(Clone, Copy, Debug)]
enum Proc {
    Laser(f64),
    Wire(f64),
    Cutter(bool),
    Galvo([f64; 2]),
    Head(u8),
    Extrude(f64),
}

/// A process run being accumulated into the next stamp.
#[derive(Clone, Copy, Debug)]
struct Run {
    start: V3,
    /// Where the process last worked.
    end: V3,
    length: f64,
    area_len: f64,
    width_len: f64,
    /// The surface under the start before this pass laid anything there (the last piece's
    /// end), if the pass has laid any.
    base: Option<f64>,
}

struct ArmSim {
    state: JointState,
    /// Setpoints on their way: (applies at, stamped at, setpoint).
    inbound: VecDeque<(f64, f64, JointCmd)>,
    /// The setpoint in force and its stamp.
    active: Option<(f64, JointCmd)>,
    last_sent: f64,
    last_arrival: f64,
    drive: DriveState,
    hold: Q,
    procs: VecDeque<(f64, Proc)>,
    laser: f64,
    wire: f64,
    cutter: bool,
    galvo: [f64; 2],
    galvo_cmd: [f64; 2],
    head_ready_at: Option<(f64, u8)>,
    /// The extruder's commanded flow and the melt reaching its nozzle (mm³/s).
    extrude: f64,
    melt: f64,
    deposit: Option<Run>,
    cut: Option<Run>,
    width_noise: f64,
    prev_tcp: Pose,
    prev_vel: V3,
    next_enc: f64,
    next_tracker: f64,
    next_imu: f64,
    next_scan: f64,
    next_pool: f64,
    last_width: f64,
    rng: Rng,
}

struct PosSim {
    truth: Positioner,
    /// Its axes and their speeds.
    q: [f64; 2],
    qd: [f64; 2],
    cmd: [f64; 2],
    inbound: VecDeque<(f64, [f64; 2])>,
    offset: [f64; 2],
    next: f64,
    rng: Rng,
}

/// The virtual cell.
pub struct SimCell {
    info: CellInfo,
    truth: Vec<ArmModel>,
    arms: Vec<ArmSim>,
    pos: Option<PosSim>,
    field: BuildField,
    cfg: SimConfig,
    bead: BeadModel,
    t: f64,
    steps: u64,
    pending: Vec<(f64, u64, Observation)>,
    order: u64,
    heartbeat: f64,
    stopped: Option<StopKind>,
    heat: Heat,
    log: SimLog,
    /// The plate's thermocouple, if the cell has a laser: its next reading and its noise.
    thermocouple: Option<(f64, Rng)>,
}

impl SimCell {
    pub fn new(setup: &CellSetup, cfg: SimConfig) -> SimCell {
        let mut rng = Rng::new(cfg.seed);
        let e = cfg.errors;
        let truth: Vec<ArmModel> = setup
            .arms
            .iter()
            .map(|a| {
                let mut t = a.clone();
                for j in t.joints.iter_mut() {
                    j.drive.stiffness *= e.stiffness;
                    j.drive.damping *= e.stiffness.sqrt();
                    j.drive.coulomb *= e.friction;
                    j.drive.preload_friction *= e.friction;
                    j.drive.viscous *= e.friction;
                }
                t.payload = t.payload.joined(&Body::block(e.payload, [0.0, 0.0, 80.0], [50.0, 50.0, 50.0]), &Pose::IDENTITY);
                t.refresh();
                t
            })
            .collect();
        let gravity = [0.0, 0.0, -GRAVITY];
        let s = cfg.sensors;
        let mut sensors = Vec::new();
        for (i, a) in setup.arms.iter().enumerate() {
            let arm = Some(ArmId(i as u8));
            let tcp = a.tool().tcp;
            sensors.push(SensorInfo { kind: SensorKind::Encoders, arm, mount: Pose::IDENTITY, hz: s.encoders.hz, latency: s.encoders.latency, sigma: s.encoders.sigma });
            sensors.push(SensorInfo { kind: SensorKind::Tracker, arm, mount: tcp, hz: s.tracker.hz, latency: s.tracker.latency, sigma: s.tracker.sigma });
            sensors.push(SensorInfo { kind: SensorKind::Imu, arm, mount: tcp, hz: s.imu.hz, latency: s.imu.latency, sigma: s.imu.sigma });
            if a.heads.iter().any(|h| matches!(h.kind, ToolKind::LaserWire { .. })) {
                sensors.push(SensorInfo { kind: SensorKind::MeltPool, arm, mount: tcp, hz: s.melt_pool.hz, latency: s.melt_pool.latency, sigma: s.melt_pool.sigma });
            }
            if let Some(h) = a.heads.iter().find(|h| h.scans()) {
                sensors.push(SensorInfo { kind: SensorKind::Scanner, arm, mount: h.tcp, hz: s.scanner.hz, latency: s.scanner.latency, sigma: s.scanner.sigma });
            }
        }
        let lasers = setup.arms.iter().flat_map(|a| a.heads.iter()).any(|h| matches!(h.kind, ToolKind::LaserWire { .. } | ToolKind::LaserCut { .. } | ToolKind::Ablation(_)));
        if lasers {
            sensors.push(SensorInfo { kind: SensorKind::Thermocouple, arm: None, mount: setup.plate, hz: s.thermocouple.hz, latency: s.thermocouple.latency, sigma: s.thermocouple.sigma });
        }
        let info = CellInfo { mode: Mode::Sim, arms: setup.arms.clone(), positioner: setup.positioner.clone(), sensors, plate: setup.plate, substrate: setup.substrate, gravity };
        assert_eq!(cfg.home.len(), truth.len(), "a home pose per arm");
        let arms = truth
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let home = cfg.home[i];
                // At rest where the motors hold home: the links sag by the true springs.
                let mut q = home;
                for _ in 0..8 {
                    let tau = static_torque(t, &frames(t, &q), gravity, &Wrench::NONE);
                    q = std::array::from_fn(|j| {
                        let d = &t.joints[j].drive;
                        home[j] - tau[j] / d.stiffness - (0.5 * d.lost_motion).copysign(tau[j])
                    });
                }
                let state = JointState { q, qd: [0.0; DOF], th: home, thd: [0.0; DOF] };
                let f = frames(t, &q);
                ArmSim {
                    state,
                    inbound: VecDeque::new(),
                    active: None,
                    last_sent: f64::MIN,
                    last_arrival: 0.0,
                    drive: DriveState::Enabled,
                    hold: home,
                    procs: VecDeque::new(),
                    laser: 0.0,
                    wire: 0.0,
                    cutter: false,
                    galvo: [0.0; 2],
                    galvo_cmd: [0.0; 2],
                    head_ready_at: None,
                    extrude: 0.0,
                    melt: 0.0,
                    deposit: None,
                    cut: None,
                    width_noise: 0.0,
                    prev_tcp: f.tcp,
                    prev_vel: ZERO,
                    next_enc: 0.0,
                    next_tracker: 0.0,
                    next_imu: 0.0,
                    next_scan: 0.0,
                    next_pool: 0.0,
                    last_width: 0.0,
                    rng: rng.fork(i as u64 + 1),
                }
            })
            .collect();
        let pos = setup.positioner.as_ref().map(|p| {
            let sigma = p.sigma;
            PosSim { truth: p.clone(), q: [0.0; 2], qd: [0.0; 2], cmd: [0.0; 2], inbound: VecDeque::new(), offset: [sigma * rng.normal(), sigma * rng.normal()], next: 0.0, rng: rng.fork(99) }
        });
        let thermocouple = lasers.then(|| (0.0, rng.fork(77)));
        let bead = BeadModel { efficiency: cfg.bead.efficiency * e.bead_area, width_k: cfg.bead.width_k * e.bead_width, ..cfg.bead };
        let mut cell = SimCell {
            info,
            truth,
            arms,
            pos,
            field: BuildField::new(cfg.field_cell),
            cfg,
            bead,
            t: 0.0,
            steps: 0,
            pending: Vec::new(),
            order: 0,
            heartbeat: 0.0,
            stopped: None,
            heat: Heat::default(),
            log: SimLog::default(),
            thermocouple,
        };
        cell.emit(0.0, Observation::Safety { at: 0.0, interlocks: Interlocks::SIM });
        cell
    }

    /// The true as-built field (part frame).
    pub fn truth_field(&self) -> &BuildField {
        &self.field
    }
    /// A part already there when the run starts (a build carried on, a test's blank): its
    /// shape goes into the true field (as it stands cold).
    pub fn place(&mut self, shape: &dyn Shape) {
        self.field.add(shape, 255);
    }
    /// The true arms.
    pub fn truth_arms(&self) -> &[ArmModel] {
        &self.truth
    }
    pub fn true_q(&self, arm: ArmId) -> Q {
        self.arms[arm.index()].state.q
    }
    /// The arm's true frames (cell frame).
    pub fn true_frames(&self, arm: ArmId) -> Frames {
        frames(&self.truth[arm.index()], &self.arms[arm.index()].state.q)
    }
    /// The part frame in the cell, as it truly is.
    pub fn true_part_frame(&self) -> Pose {
        match &self.pos {
            Some(p) => p.truth.table_frame(add2(p.q, p.offset)).mul(&self.info.plate),
            None => self.info.plate,
        }
    }
    /// The part's mean temperature above ambient (K).
    pub fn temperature(&self) -> f64 {
        self.heat.bulk
    }
    /// The part's heat as it truly stands, and the thermal model it follows (the warp:
    /// [`Thermal::warp`]).
    pub fn heat(&self) -> Heat {
        self.heat
    }
    pub fn thermal(&self) -> &Thermal {
        &self.cfg.thermal
    }
    pub fn log(&self) -> &SimLog {
        &self.log
    }
    pub fn stopped(&self) -> Option<&StopKind> {
        self.stopped.as_ref()
    }

    fn emit(&mut self, arrive: f64, obs: Observation) {
        self.order += 1;
        self.pending.push((arrive, self.order, obs));
    }

    fn step(&mut self) {
        let t = self.t + DT;
        let gravity = self.info.gravity;
        if self.stopped.is_none() && t - self.heartbeat > Watchdog::default().heartbeat && self.arms.iter().any(|a| a.active.is_some()) {
            self.stop(StopKind::Heartbeat);
        }
        let part = self.true_part_frame();
        let part_inv = part.inv();
        let (mut heat, mut at) = (0.0, 0.0);
        for i in 0..self.arms.len() {
            let (w, z) = self.step_arm(i, t, gravity, &part_inv);
            heat += w;
            at += w * z;
        }
        if let Some(p) = self.pos.as_mut() {
            while p.inbound.front().is_some_and(|c| c.0 <= t) {
                p.cmd = p.inbound.pop_front().map(|c| c.1).unwrap_or(p.cmd);
            }
            // Its servo follows the setpoint with its lag, within its speed and acceleration.
            for k in 0..2 {
                let want = ((p.cmd[k] - p.q[k]) / p.truth.lag.max(DT)).clamp(-p.truth.vmax[k], p.truth.vmax[k]);
                let dv = p.truth.accel[k] * DT;
                p.qd[k] += (want - p.qd[k]).clamp(-dv, dv);
                p.q[k] += p.qd[k] * DT;
            }
            if p.q[0] < p.truth.tilt[0] || p.q[0] > p.truth.tilt[1] {
                p.q[0] = p.q[0].clamp(p.truth.tilt[0], p.truth.tilt[1]);
                p.qd[0] = 0.0;
            }
        }
        let mut seen = None;
        if let Some(p) = self.pos.as_mut() {
            if t >= p.next {
                let s = self.cfg.sensors.positioner;
                p.next += 1.0 / s.hz;
                seen = Some((t + s.latency, [p.q[0] + s.sigma * p.rng.normal(), p.q[1] + s.sigma * p.rng.normal()]));
            }
        }
        if let Some((arrive, q)) = seen {
            self.emit(arrive, Observation::Positioner { at: t, q });
        }
        let th = self.cfg.thermal;
        let capacity = th.base_capacity + th.capacity_per_mm3 * self.log.deposited;
        self.heat = th.step(self.heat, heat, at / heat.max(1e-12), capacity, DT);
        let mut reading = None;
        if let Some((next, rng)) = self.thermocouple.as_mut() {
            if t >= *next {
                let s = self.cfg.sensors.thermocouple;
                *next += 1.0 / s.hz;
                reading = Some((t + s.latency, self.heat.bulk + s.sigma * rng.normal()));
            }
        }
        if let Some((arrive, rise)) = reading {
            self.emit(arrive, Observation::Temperature { at: t, rise });
        }
        self.t = t;
        self.steps += 1;
    }

    /// One arm's step: the controller, the plant, the process, the sensors. Returns the heat it
    /// put into the part (W) and the height it went in at (part frame).
    fn step_arm(&mut self, i: usize, t: f64, gravity: V3, part_inv: &Pose) -> (f64, f64) {
        let (th, hot) = (self.cfg.thermal, self.heat);
        let absorb = self.cfg.errors.absorb;
        let truth = &self.truth[i];
        let c = truth.controller;
        let a = &mut self.arms[i];
        while a.inbound.front().is_some_and(|s| s.0 <= t) {
            let (arrive, at, cmd) = a.inbound.pop_front().expect("a setpoint");
            if a.drive == DriveState::Enabled {
                a.active = Some((at, cmd));
                a.last_arrival = arrive;
            }
        }
        while a.procs.front().is_some_and(|p| p.0 <= t) {
            let (_, p) = a.procs.pop_front().expect("a process command");
            match p {
                Proc::Laser(w) => a.laser = w,
                Proc::Wire(f) => a.wire = f,
                Proc::Cutter(on) => a.cutter = on,
                Proc::Galvo(g) => a.galvo_cmd = g,
                Proc::Extrude(q) => a.extrude = q,
                Proc::Head(h) => {
                    let h = (h as usize).min(truth.heads.len() - 1);
                    a.head_ready_at = Some((t + truth.heads[h].switch, h as u8));
                    a.laser = 0.0;
                    a.wire = 0.0;
                    a.cutter = false;
                    a.extrude = 0.0;
                }
            }
        }
        let mut ready = None;
        if let Some((when, h)) = a.head_ready_at {
            if t >= when {
                a.head_ready_at = None;
                ready = Some(h);
            }
        }
        if let Some(h) = ready {
            self.truth[i].set_head(h as usize);
        }
        let truth = &self.truth[i];
        let nominal = &self.info.arms[i];
        let a = &mut self.arms[i];
        // The controller's command for this step.
        if a.drive == DriveState::Enabled && a.active.is_some() && t - a.last_arrival > c.watchdog {
            a.drive = DriveState::Fault(DriveFault::Watchdog);
            a.hold = a.state.th;
            self.log.faults.push((t, ArmId(i as u8), DriveFault::Watchdog));
        }
        let held = a.drive != DriveState::Enabled || a.active.is_none();
        let mut cmd = match (held, a.active) {
            (false, Some((at, sp))) => {
                let since = (t - at - c.latency).max(0.0);
                let th = match c.interp {
                    Interp::Hold => sp.th,
                    Interp::Velocity => std::array::from_fn(|j| sp.th[j] + sp.thd[j] * since.min(2.0 / c.setpoint_hz)),
                };
                DriveCmd { th, thd: sp.thd, tau: sp.tau, kp: sp.kp, kd: sp.kd }
            }
            _ => DriveCmd { tau: [0.0; DOF], ..DriveCmd::hold(truth, a.hold) },
        };
        if held || matches!(c.servo, ServoLoop::Vendor { .. }) {
            // The maker's loop: its gains, its own gravity model at its encoders.
            let g = static_torque(nominal, &frames(nominal, &a.state.th), gravity, &Wrench::NONE);
            cmd.tau = g;
            cmd.kp = std::array::from_fn(|j| truth.joints[j].drive.kp);
            cmd.kd = std::array::from_fn(|j| truth.joints[j].drive.kd);
        }
        if held {
            cmd.thd = [0.0; DOF];
        }
        // The environment on the tool: the cable bundle.
        let f0 = frames(truth, &a.state.q);
        let mut wrench = Wrench::NONE;
        if let Some(cable) = truth.tool().cable {
            let at = f0.flange.apply(cable.attach);
            let force = cable.force(at, truth.base.apply(cable.anchor));
            wrench = Wrench { force, moment: cross(sub(at, f0.tcp.t), force) };
        }
        let info = step(truth, &mut a.state, &cmd, gravity, &wrench, DT);
        if info.saturated {
            self.log.saturated += 1;
        }
        // Following error and limits fault the drive.
        if a.drive == DriveState::Enabled && !held {
            let fault = (0..DOF).find_map(|j| {
                if (cmd.th[j] - a.state.th[j]).abs() > c.follow_max {
                    Some(DriveFault::Following { joint: j as u8 })
                } else if a.state.q[j] < truth.joints[j].lo - 0.02 || a.state.q[j] > truth.joints[j].hi + 0.02 {
                    Some(DriveFault::Limit { joint: j as u8 })
                } else {
                    None
                }
            });
            if let Some(fault) = fault {
                a.drive = DriveState::Fault(fault);
                a.hold = a.state.th;
                self.log.faults.push((t, ArmId(i as u8), fault));
            }
        }
        let f = frames(truth, &a.state.q);
        // The galvo's mirrors follow their command.
        if let Some(g) = truth.tool().galvo {
            let k = 1.0 - (-std::f64::consts::TAU * g.bandwidth * DT).exp();
            for j in 0..2 {
                a.galvo[j] += (a.galvo_cmd[j].clamp(-g.range, g.range) - a.galvo[j]) * k;
            }
        }
        let tcp_part = part_inv.mul(&f.tcp);
        // The true field holds the part cold: what the tools meet stands where the heat warps it.
        let cold = |p: V3| th.cold(&hot, p);
        let heat_z = cold(tcp_part.t)[2];
        let vel = scale(sub(f.tcp.t, a.prev_tcp.t), 1.0 / DT);
        let speed = len(sub(tcp_part.t, part_inv.mul(&a.prev_tcp).t)) / DT;
        let mut heat = 0.0;
        let tool = truth.tool().clone();
        let enabled = a.drive == DriveState::Enabled && a.head_ready_at.is_none();
        // Deposition.
        let depositing = enabled && tool.deposits() && a.laser > 0.0 && a.wire > 0.0;
        let mut beads: Vec<Bead> = Vec::new();
        // The plastic laid this step (mm³): an extruder's beads carry their melt's volume.
        let mut plastic = 0.0;
        let mut lof = 0;
        let mut strings = 0;
        if let ToolKind::LaserWire { wire_mm, .. } = tool.kind {
            if depositing {
                let capture = tool.galvo.map_or(0.0, |g| g.capture);
                let beam = cold(add(tcp_part.t, tcp_part.rotate([a.galvo[0] * capture, a.galvo[1] * capture, 0.0])));
                let tau = self.cfg.errors.noise_time.max(DT);
                let k = (-DT / tau).exp();
                a.width_noise = a.width_noise * k + (1.0 - k * k).sqrt() * a.rng.normal();
                let area = self.bead.area(wire_mm, a.wire, speed);
                let width = self.bead.width(a.laser, speed) * (1.0 + self.cfg.errors.width_noise * a.width_noise);
                a.last_width = width;
                heat += th.absorb * absorb * a.laser;
                let run = a.deposit.get_or_insert(Run { start: beam, end: beam, length: 0.0, area_len: 0.0, width_len: 0.0, base: None });
                let step_len = (speed * DT).max(self.bead.min_speed * DT);
                run.end = beam;
                run.length += step_len;
                run.area_len += area * step_len;
                run.width_len += width * step_len;
                if dist(run.start, beam) >= piece(&self.field) {
                    let r = *run;
                    let (b, gap) = lay(&self.field, &self.info.substrate, &r, beam, wire_mm);
                    a.deposit = Some(Run { start: beam, end: beam, length: 0.0, area_len: 0.0, width_len: 0.0, base: Some(b.b[2]) });
                    lof += gap as u32;
                    beads.push(b);
                }
            } else if let Some(r) = a.deposit.take() {
                // What is left of the run ends where the beam last laid it.
                if r.length > 0.0 {
                    let (b, gap) = lay(&self.field, &self.info.substrate, &r, r.end, wire_mm);
                    lof += gap as u32;
                    beads.push(b);
                }
            }
        }
        // Extrusion: the melt follows the drive with its lag, at the true flow (a drive run
        // backwards pulls it back: a retraction); the bead takes the gap under the nozzle as
        // its height, as far as its own strand reaches.
        if let ToolKind::Extruder(e) = tool.kind {
            let lag = (e.lag * self.cfg.errors.melt_lag).max(DT);
            let want = if enabled { (a.extrude * self.cfg.errors.flow).clamp(-e.flow_max, e.flow_max) } else { 0.0 };
            a.melt += (want - a.melt) * (1.0 - (-DT / lag).exp());
            let tip = cold(tcp_part.t);
            if a.melt > 1e-3 {
                let run = a.deposit.get_or_insert(Run { start: tip, end: tip, length: 0.0, area_len: 0.0, width_len: 0.0, base: None });
                run.end = tip;
                run.length += speed * DT;
                // area_len holds the melt's volume (mm³).
                run.area_len += a.melt * DT;
                if dist(run.start, tip) >= piece(&self.field) {
                    let r = *run;
                    let (b, unfused) = squeeze(&self.field, &self.info.substrate, &r, tip);
                    a.deposit = Some(Run { start: tip, end: tip, length: 0.0, area_len: 0.0, width_len: 0.0, base: Some(b.b[2]) });
                    let ooze = a.extrude <= 0.0;
                    lof += (unfused && !ooze) as u32;
                    strings += ooze as u32;
                    plastic += r.area_len;
                    beads.push(b);
                }
            } else if let Some(r) = a.deposit.take() {
                if r.area_len > 1e-6 {
                    let (b, unfused) = squeeze(&self.field, &self.info.substrate, &r, r.end);
                    let ooze = a.extrude <= 0.0;
                    lof += (unfused && !ooze) as u32;
                    strings += ooze as u32;
                    plastic += r.area_len;
                    beads.push(b);
                }
            }
        }
        // Laser cutting.
        let mut slabs: Vec<Slab> = Vec::new();
        let mut cut_log = None;
        let mut plate_hit = false;
        if let ToolKind::LaserCut { kerf, standoff, defocus, gas, height_control } = tool.kind {
            let cutting = enabled && a.cutter;
            if cutting {
                heat += th.absorb_cut * absorb * th.cut_power;
                let run = a.cut.get_or_insert(Run { start: tcp_part.t, end: tcp_part.t, length: 0.0, area_len: 0.0, width_len: 0.0, base: None });
                run.length += speed * DT;
                if dist(run.start, tcp_part.t) >= self.field.cell() {
                    let start = run.start;
                    a.cut = Some(Run { start: tcp_part.t, end: tcp_part.t, length: 0.0, area_len: 0.0, width_len: 0.0, base: None });
                    // The hot part is cut where it stands: in the cold part the beam sits where
                    // the warp takes it back to.
                    let (a0, b0) = (cold(start), cold(tcp_part.t));
                    let mid = lerp(a0, b0, 0.5);
                    let along = dist(a0, b0);
                    let (x, y, z) = (tcp_part.x(), col(&tcp_part.r, 1), tcp_part.z());
                    // No height control: the kerf widens as the focus wanders (estimate).
                    let kerf = if height_control { kerf } else { kerf * 1.5 };
                    let r = [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]];
                    let beam_mid = (defocus - standoff) * 0.5;
                    slabs.push(Slab { centre: add(mid, scale(z, beam_mid)), r, half: [0.5 * kerf, 0.5 * (along + kerf), 0.5 * (standoff + defocus)] });
                    // The offcut: material within its thickness outside the kerf, as far along
                    // the beam as it runs from the focus.
                    let o = self.cfg.cut.offcut;
                    let side = add(mid, scale(x, 0.5 * kerf + 0.5 * o));
                    let mut run_end = None;
                    let mut s = 0.0;
                    while s <= defocus {
                        if self.field.distance(add(side, scale(z, s))) < 0.5 * o {
                            run_end = Some(s);
                        } else if run_end.is_some() {
                            break;
                        }
                        s += self.field.cell();
                    }
                    if let Some(end) = run_end {
                        let half_z = 0.5 * (end + standoff);
                        slabs.push(Slab { centre: add(side, scale(z, half_z - standoff)), r, half: [0.5 * o, 0.5 * (along + kerf), half_z] });
                        cut_log = Some((mid, z, along, end, self.cfg.cut.recast(gas), self.cfg.cut.dross(gas)));
                    } else {
                        cut_log = Some((mid, z, along, 0.0, self.cfg.cut.recast(gas), self.cfg.cut.dross(gas)));
                    }
                    // The substrate within the beam's damaging range.
                    let sub_ = self.info.substrate;
                    plate_hit = (1..=12).any(|k| sub_.distance(add(mid, scale(z, defocus * k as f64 / 12.0))) < 0.0);
                }
            } else {
                a.cut = None;
            }
        }
        // Sensors.
        let s = self.cfg.sensors;
        let id = ArmId(i as u8);
        let mut out: Vec<(f64, Observation)> = Vec::new();
        if t >= a.next_enc {
            a.next_enc += 1.0 / s.encoders.hz;
            let th = std::array::from_fn(|j| a.state.th[j] + s.encoders.sigma * a.rng.normal());
            out.push((t + s.encoders.latency, Observation::Joints { arm: id, at: t, th, thd: a.state.thd, tau: info.tau_motor }));
        }
        if t >= a.next_tracker {
            a.next_tracker += 1.0 / s.tracker.hz;
            let n = [a.rng.normal(), a.rng.normal(), a.rng.normal()];
            let w = [a.rng.normal(), a.rng.normal(), a.rng.normal()];
            let r = mm(&rot_axis(norm(add(w, [1e-12, 0.0, 0.0])), s.tracker_rot * len(w)), &f.tcp.r);
            out.push((t + s.tracker.latency, Observation::Tracker { arm: id, at: t, tcp: Pose::new(r, add(f.tcp.t, scale(n, s.tracker.sigma))) }));
        }
        if t >= a.next_imu {
            a.next_imu += 1.0 / s.imu.hz;
            let acc = scale(sub(vel, a.prev_vel), 1.0 / DT);
            let specific = mtv(&f.tcp.r, sub(acc, gravity));
            let w = scale(log_rot(&mm(&f.tcp.r, &transpose(&a.prev_tcp.r))), 1.0 / DT);
            let gyro = mtv(&f.tcp.r, w);
            let accel = std::array::from_fn(|k| specific[k] + s.imu.sigma * a.rng.normal());
            let gyro = std::array::from_fn(|k| gyro[k] + s.gyro * a.rng.normal());
            out.push((t + s.imu.latency, Observation::Imu { arm: id, at: t, accel, gyro }));
        }
        if depositing && t >= a.next_pool {
            a.next_pool = t + 1.0 / s.melt_pool.hz;
            out.push((t + s.melt_pool.latency, Observation::BeadWidth { arm: id, at: t, width: a.last_width + s.melt_pool.sigma * a.rng.normal() }));
        }
        let scanning = enabled && tool.scans();
        if scanning && t >= a.next_scan {
            a.next_scan = t + 1.0 / s.scanner.hz;
            if let ToolKind::Scanner { width, range } = tool.kind {
                let points = scan(&self.field, &self.info.substrate, &tcp_part, width, range, s.scan_points, (&th, &hot), s.scanner.sigma, &mut a.rng);
                out.push((t + s.scanner.latency, Observation::Profile { arm: id, at: t, points }));
            }
        }
        a.prev_vel = vel;
        a.prev_tcp = f.tcp;
        let drive = a.drive;
        let changed = ready;
        // Apply the process to the true field.
        for b in &beads {
            if plastic == 0.0 {
                let l = dist(b.a, b.b).max(1e-3);
                self.log.deposited += b.width * b.height / 1.5 * l;
            }
            self.field.add(&b.shape(), 255);
            self.log.beads.push((t, *b));
        }
        self.log.deposited += plastic;
        self.log.lack_of_fusion += lof;
        self.log.strings += strings;
        for sl in &slabs {
            self.field.remove(sl, 255);
        }
        if let Some((mid, z, along, depth, recast, dross)) = cut_log {
            self.log.cuts.push((t, mid, z));
            self.log.cut_length += along;
            self.log.recast += recast * along;
            self.log.dross += dross * depth * along;
        }
        if plate_hit {
            self.log.plate_hits += 1;
        }
        for (arrive, o) in out {
            self.emit(arrive, o);
        }
        if let Some(h) = changed {
            self.emit(t + 0.001, Observation::HeadReady { arm: id, at: t, head: h });
        }
        if let DriveState::Fault(_) = drive {
            if self.log.faults.last().is_some_and(|f| f.0 == t) {
                self.emit(t + 0.001, Observation::Drive { arm: id, at: t, state: drive });
            }
        }
        (heat, heat_z)
    }

    fn stop(&mut self, why: StopKind) {
        for a in self.arms.iter_mut() {
            a.laser = 0.0;
            a.wire = 0.0;
            a.cutter = false;
            a.extrude = 0.0;
            a.hold = a.state.th;
            a.active = None;
            a.inbound.clear();
        }
        self.stopped = Some(why);
    }
}

fn add2(a: [f64; 2], b: [f64; 2]) -> [f64; 2] {
    [a[0] + b[0], a[1] + b[1]]
}

/// How long a run gets before the cell lays it as one bead (mm).
fn piece(field: &BuildField) -> f64 {
    4.0 * field.cell()
}

/// The bead a run lays from its start to `end` (part frame): on the surface under the wire.
/// Also whether the wire was out of reach of it (more than two wire thicknesses above).
fn lay(field: &BuildField, substrate: &Substrate, r: &Run, end: V3, wire_mm: f64) -> (Bead, bool) {
    let l = r.length.max(1e-9);
    let (area, width) = (r.area_len / l, r.width_len / l);
    let height = BeadModel::height(area, width);
    let under = |p: V3| {
        let floor = substrate.top(p[0], p[1]);
        let top = field.top_below(add(p, [0.0, 0.0, 3.0]), 8.0).unwrap_or(floor).max(floor);
        (top, p[2] - top)
    };
    // The bead sits on what was there before this pass: under its start, where the last piece
    // ended (that piece now covers it); under its end, ahead of everything laid so far, unless
    // this is the short rest of the run, ending inside the last piece's round end.
    let (ta, ga) = match r.base {
        Some(t) => (t, r.start[2] - t),
        None => under(r.start),
    };
    let (tb, gb) = match r.base {
        Some(t) if dist(r.start, end) < piece(field) => (t, end[2] - t),
        _ => under(end),
    };
    let gap = ga.max(gb) > 2.0 * wire_mm + 1.0;
    (Bead { a: [r.start[0], r.start[1], ta], b: [end[0], end[1], tb], width, height, up: [0.0, 0.0, 1.0] }, gap)
}

/// The plastic bead a run lays from its start to `end` (part frame): its melt's volume over
/// the run, squeezed into the gap between the nozzle and the surface under it (the gap is its
/// height, at most the round strand the melt makes when nothing squeezes it). Also whether
/// the strand fell from higher than its own thickness (no bond).
fn squeeze(field: &BuildField, substrate: &Substrate, r: &Run, end: V3) -> (Bead, bool) {
    let under = |p: V3| {
        let floor = substrate.top(p[0], p[1]);
        field.top_below(add(p, [0.0, 0.0, 0.5]), 8.0).unwrap_or(floor).max(floor)
    };
    let (ta, tb) = (r.base.unwrap_or_else(|| under(r.start)), under(end));
    let along = dist([r.start[0], r.start[1], 0.0], [end[0], end[1], 0.0]);
    // Standing still, the melt piles up round the nozzle: a blob a strand long.
    let length = along.max(r.length).max(0.25);
    let area = r.area_len / length;
    let strand = (4.0 * area / std::f64::consts::PI).sqrt();
    let gap = 0.5 * ((r.start[2] - ta) + (end[2] - tb));
    let height = gap.clamp(0.05 * strand, strand);
    let width = bead_width(area, height).max(height);
    (Bead { a: [r.start[0], r.start[1], ta], b: [end[0], end[1], tb], width, height, up: [0.0, 0.0, 1.0] }, gap > 2.0 * strand)
}

/// A line scan: `n` rays across the scanner's x over `width`, along its z within ±`range` of
/// its focus, against the hot part (the cold field warped by `heat`) and the plate. The hits in
/// the scanner's frame, with noise.
#[allow(clippy::too_many_arguments)]
fn scan(field: &BuildField, substrate: &Substrate, at: &Pose, width: f64, range: f64, n: usize, heat: (&Thermal, &Heat), sigma: f64, rng: &mut Rng) -> Vec<V3> {
    let (th, h) = heat;
    let scene = |p: V3| {
        let cold = th.cold(h, p);
        (field.distance(cold) * th.growth(h, cold[2])).min(substrate.distance(p))
    };
    let (x, z) = (at.x(), at.z());
    let mut pts = Vec::with_capacity(n);
    for k in 0..n {
        let u = (k as f64 / (n - 1).max(1) as f64 - 0.5) * width;
        let o = add(add(at.t, scale(x, u)), scale(z, -range));
        let mut s = 0.0;
        while s < 2.0 * range {
            let d = scene(add(o, scale(z, s)));
            if d < 0.02 {
                let hit = add(o, scale(z, s));
                let local = mtv(&at.r, sub(hit, at.t));
                pts.push([local[0], local[1], local[2] + sigma * rng.normal()]);
                break;
            }
            s += d.max(0.02);
        }
    }
    pts
}

impl CellLink for SimCell {
    fn info(&self) -> &CellInfo {
        &self.info
    }

    fn now(&self) -> Stamp {
        self.t
    }

    fn send(&mut self, cmd: Command) -> Result<(), LinkError> {
        if let Some(why) = &self.stopped {
            if !matches!(cmd, Command::Heartbeat { .. }) {
                return Err(LinkError::Refused(format!("stopped: {why:?}")));
            }
        }
        let arm_of = |a: ArmId| if a.index() < self.arms.len() { Ok(a.index()) } else { Err(LinkError::Refused(format!("no arm {}", a.0))) };
        match cmd {
            Command::Joints { arm, at, cmd } => {
                let i = arm_of(arm)?;
                let c = self.truth[i].controller;
                let a = &mut self.arms[i];
                if let DriveState::Fault(f) = a.drive {
                    return Err(LinkError::Refused(format!("drive fault {f:?}")));
                }
                // Faster than the controller takes: dropped, as the maker's does.
                if at - a.last_sent < 0.999 / c.setpoint_hz_max {
                    self.log.dropped += 1;
                    return Ok(());
                }
                a.last_sent = at;
                let jitter = (c.jitter * a.rng.normal()).abs();
                let apply = at.max(self.t) + c.latency + jitter;
                // A late setpoint never overtakes an earlier one.
                let apply = a.inbound.back().map_or(apply, |b| apply.max(b.0));
                a.inbound.push_back((apply, at, cmd));
            }
            Command::Laser { arm, at, watts } => {
                let i = arm_of(arm)?;
                self.arms[i].procs.push_back((at.max(self.t) + 0.001, Proc::Laser(watts)));
            }
            Command::Wire { arm, at, mm_s } => {
                let i = arm_of(arm)?;
                self.arms[i].procs.push_back((at.max(self.t) + 0.001, Proc::Wire(mm_s)));
            }
            Command::Extrude { arm, at, mm3_s } => {
                let i = arm_of(arm)?;
                self.arms[i].procs.push_back((at.max(self.t) + 0.001, Proc::Extrude(mm3_s)));
            }
            Command::Galvo { arm, at, offset } => {
                let i = arm_of(arm)?;
                let lat = self.truth[i].tool().galvo.map_or(0.001, |g| g.latency);
                self.arms[i].procs.push_back((at.max(self.t) + lat, Proc::Galvo(offset)));
            }
            Command::Head { arm, at, head } => {
                let i = arm_of(arm)?;
                self.arms[i].procs.push_back((at.max(self.t) + 0.001, Proc::Head(head)));
            }
            Command::Cutter { arm, at, on } => {
                let i = arm_of(arm)?;
                self.arms[i].procs.push_back((at.max(self.t) + 0.001, Proc::Cutter(on)));
            }
            Command::Positioner { at, q, .. } => {
                let Some(p) = self.pos.as_mut() else { return Err(LinkError::Refused("no positioner".into())) };
                p.inbound.push_back((at.max(self.t) + p.truth.latency, q));
            }
            Command::Heartbeat { at } => self.heartbeat = self.heartbeat.max(at),
            Command::Stop { why, .. } => self.stop(why),
        }
        Ok(())
    }

    fn advance(&mut self, until: Stamp, out: &mut Vec<Observation>) {
        while self.t + 0.5 * DT < until {
            self.step();
        }
        let now = self.t;
        let mut ready: Vec<(f64, u64, Observation)> = Vec::new();
        let mut i = 0;
        while i < self.pending.len() {
            if self.pending[i].0 <= now + 1e-9 {
                ready.push(self.pending.swap_remove(i));
            } else {
                i += 1;
            }
        }
        ready.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        out.extend(ready.into_iter().map(|r| r.2));
    }
}

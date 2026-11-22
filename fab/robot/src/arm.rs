//! One 6-axis arm as the planner and the simulator see it: its kinematic chain (each joint's
//! placement on its parent link and its axis, the URDF form; DH tables convert into it), each
//! link's mass, centre of mass and inertia, each joint's drive (gear, rotor, elasticity,
//! friction, torque, servo gains), its controller (setpoint rate, latency, interpolation,
//! limits and fault rules), the tool on its flange and the cable dragging on that tool.
//!
//! Units: mm, kg, s, rad. A force is kg·mm/s² ([`NEWTON`] of them make a newton), a torque
//! kg·mm²/s² ([`NEWTON_METRE`]), a joint stiffness kg·mm²/s² per rad.

use crate::math::*;

pub const DOF: usize = 6;
/// One value per joint.
pub type Q = [f64; DOF];

/// Standard gravity (mm/s²).
pub const GRAVITY: f64 = 9806.65;
/// One newton in the crate's force unit (kg·mm/s²).
pub const NEWTON: f64 = 1e3;
/// One newton metre in the crate's torque unit (kg·mm²/s²).
pub const NEWTON_METRE: f64 = 1e6;
/// One kg·m² in kg·mm².
pub const KG_M2: f64 = 1e6;

/// A rigid body in its link's frame: mass, centre of mass, inertia about the centre of mass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Body {
    pub mass: f64,
    pub com: V3,
    pub inertia: M3,
}

impl Body {
    pub const NONE: Body = Body { mass: 0.0, com: ZERO, inertia: [[0.0; 3]; 3] };

    /// A solid box `size` (mm) of `mass` centred at `com`.
    pub fn block(mass: f64, com: V3, size: V3) -> Body {
        let [x, y, z] = size;
        let k = mass / 12.0;
        Body { mass, com, inertia: [[k * (y * y + z * z), 0.0, 0.0], [0.0, k * (x * x + z * z), 0.0], [0.0, 0.0, k * (x * x + y * y)]] }
    }

    /// `self` and `other` (given in the frame `at` of `self`'s frame) as one body.
    pub fn joined(&self, other: &Body, at: &Pose) -> Body {
        let m = self.mass + other.mass;
        if m <= 0.0 {
            return Body::NONE;
        }
        let oc = at.apply(other.com);
        let com = scale(add(scale(self.com, self.mass), scale(oc, other.mass)), 1.0 / m);
        // Each inertia turned into this frame, then moved to the joint centre (parallel axes).
        let io = mm(&mm(&at.r, &other.inertia), &transpose(&at.r));
        let shift = |b: &M3, mass: f64, d: V3| -> M3 {
            let dd = dot(d, d);
            std::array::from_fn(|i| std::array::from_fn(|j| b[i][j] + mass * (if i == j { dd } else { 0.0 } - d[i] * d[j])))
        };
        let inertia = madd(&shift(&self.inertia, self.mass, sub(self.com, com)), &shift(&io, other.mass, sub(oc, com)));
        Body { mass: m, com, inertia }
    }
}

/// How a joint is driven, link side (torques and stiffness as the joint sees them).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Drive {
    /// Reduction: motor turns per joint turn.
    pub gear: f64,
    /// Rotor inertia at the motor (kg·mm²); the joint sees it times `gear²`.
    pub rotor: f64,
    /// Elasticity between motor and link: gear, bearings, structure (torque per rad).
    pub stiffness: f64,
    /// Damping across that elasticity (torque per rad/s).
    pub damping: f64,
    /// Coulomb friction (torque) and viscous friction (torque per rad/s), on the motor side.
    pub coulomb: f64,
    pub viscous: f64,
    /// What the drive gives continuously (torque): holding loads past it overheats it.
    pub torque_rated: f64,
    /// The most the drive gives (torque).
    pub torque_peak: f64,
    /// The drive's servo gains on the motor angle (torque per rad, per rad/s): what an
    /// impedance-style command asks for unless it names its own.
    pub kp: f64,
    pub kd: f64,
    /// Lost motion between the motor and the link (rad, the whole band): within it the
    /// gear transmits nothing (backlash, a strain-wave gear's hysteresis). A motor-side
    /// encoder cannot see it.
    pub lost_motion: f64,
    /// A bias torque on the link (torque, signed: the way it pushes), from a spring or a
    /// constant-force cable: it keeps the gear on one flank, so a load that reverses by less
    /// never opens the lost motion ([`crate::dynamics::aim_preload`] points it).
    pub preload: f64,
    /// The Coulomb friction the preload adds at the motor (torque): the mesh always carries it.
    pub preload_friction: f64,
    /// Which of these are our estimates rather than the maker's figures.
    pub estimated: Estimated,
}

impl Drive {
    /// The Coulomb friction at the motor, the preload's share included (torque).
    pub fn coulomb_total(&self) -> f64 {
        self.coulomb + self.preload_friction
    }

    /// The rotor inertia as the joint feels it.
    pub fn reflected(&self) -> f64 {
        self.rotor * self.gear * self.gear
    }

    /// How far the gear is wound up at motor-minus-link angle `twist`: the twist past half
    /// the lost-motion band (zero inside it).
    pub fn windup(&self, twist: f64) -> f64 {
        let b = 0.5 * self.lost_motion;
        if twist > b {
            twist - b
        } else if twist < -b {
            twist + b
        } else {
            0.0
        }
    }
}

/// Where a figure comes from: published by the maker (or a source the research note cites),
/// measured by the user on their own hardware, or our estimate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tag {
    Published,
    Measured,
    Estimated,
}

impl Tag {
    pub fn letter(self) -> &'static str {
        match self {
            Tag::Published => "P",
            Tag::Measured => "M",
            Tag::Estimated => "E",
        }
    }
}

/// One line of a preset's sheet: the parameter, its value, where it comes from.
pub type SheetRow = (&'static str, &'static str, Tag);

/// Which drive figures are estimates (no published value).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Estimated {
    pub gear: bool,
    pub rotor: bool,
    pub stiffness: bool,
    pub friction: bool,
    pub torque: bool,
    pub gains: bool,
}

impl Estimated {
    pub const ALL: Estimated = Estimated { gear: true, rotor: true, stiffness: true, friction: true, torque: true, gains: true };
}

/// One revolute joint: where it sits on its parent link and what it turns about.
#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    pub name: String,
    /// The parent link's frame to this joint's frame at zero angle.
    pub origin: Pose,
    /// The axis it turns about, unit, in its own frame.
    pub axis: V3,
    pub lo: f64,
    pub hi: f64,
    /// Fastest joint speed (rad/s).
    pub vmax: f64,
    pub drive: Drive,
}

/// How the setpoints a drive receives are filled between arrivals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interp {
    /// The last setpoint is held until the next one (impedance motors latch a command).
    Hold,
    /// Carried along the commanded velocity until the next one.
    Velocity,
}

/// Who closes the joints' position loop.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ServoLoop {
    /// Ours: the drives take motor angle, speed, feed-forward torque and gains per setpoint and
    /// run only their current loop around them; feed-forward, gains and damping are the planner's.
    Own,
    /// The arm's own controller: it takes joint position setpoints only, holds gravity with its
    /// own model of the arm and the payload it was told of, and tracks with its internal position
    /// loop (the drives' `kp`, `kd`: a loop of `bandwidth` Hz at damping ratio `zeta`). The
    /// planner closes only an outer loop, on the tracker and the IMU.
    Vendor { bandwidth: f64, zeta: f64 },
}

/// How the arm's controller behaves: what the planner must respect and what the simulator
/// reproduces.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Controller {
    /// Setpoints per second the servo interface takes (the planner streams at this rate).
    pub setpoint_hz: f64,
    /// The most setpoints per second the link can carry (a bus shared by every motor).
    pub setpoint_hz_max: f64,
    /// From a setpoint leaving the planner until the drive applies it (s), and its jitter
    /// (one sigma, s).
    pub latency: f64,
    pub jitter: f64,
    pub interp: Interp,
    /// Joint acceleration and jerk the controller accepts (rad/s², rad/s³).
    pub accel: Q,
    pub jerk: Q,
    /// A following error past this (rad, motor angle against its setpoint) faults the drive.
    pub follow_max: f64,
    /// No setpoint for this long (s) and the drive stops in place.
    pub watchdog: f64,
    pub servo: ServoLoop,
}

/// What the tool does at the part.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ToolKind {
    /// A laser with wire fed into its melt pool: deposits metal, `rate` kg/h of steel on
    /// `power` kW of beam.
    LaserWire { wire_mm: f64, standoff: f64, power: f64, rate: f64 },
    /// A laser cutting head: trims without touching (no process force). Past the cut the beam
    /// diverges; it still damages what it meets within `defocus` mm of the cut. Without height
    /// control (a handheld-class head) its standoff is wherever the arm holds it.
    LaserCut { kerf: f64, standoff: f64, defocus: f64, gas: AssistGas, height_control: bool },
    /// A milling spindle: trims by cutting, with a reaction force.
    Spindle { radius: f64, flute: f64 },
    /// A pulsed laser under a scan head: removes thin layers by ablation while the arm holds
    /// still at a station and the mirrors scan the region.
    Ablation(Ablation),
    /// A line scanner as the arm's work: it measures, it does not touch (`width` of its line,
    /// `range` its depth of field, mm).
    Scanner { width: f64, range: f64 },
    /// A plastic extruder: lays beads of melt the nozzle squeezes onto the layer below.
    Extruder(Extruder),
}

/// What an extruder melts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Feed {
    /// Filament of this diameter (mm), pushed by a drive gear.
    Filament { diameter: f64 },
    /// Pellets, melted and pushed by a screw.
    Pellets,
}

/// A hot end and its nozzle. The melt reaching the nozzle follows the commanded flow as a
/// first-order lag (the melt chamber's compliance); the bead takes the gap between the nozzle
/// and the surface below as its height and spreads to hold the volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Extruder {
    /// The nozzle's bore (mm).
    pub nozzle: f64,
    pub feed: Feed,
    /// The most melt it delivers (mm³/s).
    pub flow_max: f64,
    /// The melt's lag behind the commanded flow (s).
    pub lag: f64,
    /// A layer must take at least this long per mm of its height to cool (s/mm).
    pub cool_per_mm: f64,
}

impl Extruder {
    /// The layer height and line width it prints at: 0.6 and 1.15 of the nozzle (1.0 mm nozzle:
    /// 0.6 mm layers, 1.15 mm lines).
    pub fn layer(&self) -> f64 {
        0.6 * self.nozzle
    }
    pub fn line(&self) -> f64 {
        1.15 * self.nozzle
    }
    /// The shortest time a layer `height` mm high may take.
    pub fn min_layer_time(&self, height: f64) -> f64 {
        self.cool_per_mm * height
    }
}

/// A bead's section (mm²) at `width` and `height`: a rectangle with half-round sides, the
/// slicer's flow model.
pub fn bead_area(width: f64, height: f64) -> f64 {
    (width - height).max(0.0) * height + 0.25 * std::f64::consts::PI * height * height
}

/// The width a bead `height` high spreads to with section `area` (inverse of [`bead_area`]).
pub fn bead_width(area: f64, height: f64) -> f64 {
    if height <= 1e-9 {
        return 0.0;
    }
    ((area - 0.25 * std::f64::consts::PI * height * height) / height).max(0.0) + height
}

/// Pulsed-laser ablation: the scan head and the removal it makes. A pulse at fluence F above
/// the threshold Fth takes `penetration · ln(F / Fth)` deep over its spot; pulses `scan / rep_hz`
/// apart on lines `hatch` apart stack into the depth one pass takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ablation {
    /// The scan head's field (mm square, centred on the tool axis at focus).
    pub field: f64,
    /// How far from focus it still ablates as planned (± mm along the beam).
    pub focus_depth: f64,
    /// Pulse fluence at focus and the material's threshold (J/cm²).
    pub fluence: f64,
    pub threshold: f64,
    /// Effective optical penetration depth (mm).
    pub penetration: f64,
    /// Spot diameter (mm), pulses per second, scan speed (mm/s), line spacing (mm).
    pub spot: f64,
    pub rep_hz: f64,
    pub scan: f64,
    pub hatch: f64,
    /// What a pass leaves on the face: recast layer thickness and roughness Ra (mm).
    pub recast: f64,
    pub ra: f64,
}

impl Ablation {
    pub fn depth_per_pulse(&self) -> f64 {
        if self.fluence <= self.threshold {
            0.0
        } else {
            self.penetration * (self.fluence / self.threshold).ln()
        }
    }

    /// The depth one pass takes off (mm).
    pub fn depth_per_pass(&self) -> f64 {
        let area = std::f64::consts::PI * 0.25 * self.spot * self.spot;
        self.depth_per_pulse() * area * self.rep_hz / (self.scan * self.hatch)
    }

    /// The material removal rate (mm³/s).
    pub fn removal_rate(&self) -> f64 {
        self.depth_per_pass() * self.scan * self.hatch
    }

    /// The time one pass over `area` mm² takes (s).
    pub fn pass_time(&self, area: f64) -> f64 {
        area / (self.scan * self.hatch)
    }
}

/// The cutting laser's assist gas: nitrogen leaves a clean, oxide-free face and needs more
/// power; oxygen burns along, cuts faster, and leaves an oxide layer and more dross.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssistGas {
    Nitrogen,
    Oxygen,
}

/// The hoses and cables (fibre, gas, wire conduit) pulling on a tool: a hanging weight plus a
/// spring toward where they come from.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cable {
    /// Where the bundle grips the tool (flange frame).
    pub attach: V3,
    /// Where it comes from (arm base frame).
    pub anchor: V3,
    /// Pull toward the anchor per mm of distance (force per mm).
    pub stiffness: f64,
    /// The part of the bundle the tool carries (kg).
    pub weight: f64,
}

impl Cable {
    /// The force the cable puts on the tool at `attach_world` (base anchor at `anchor_world`).
    pub fn force(&self, attach_world: V3, anchor_world: V3) -> V3 {
        let pull = scale(sub(anchor_world, attach_world), self.stiffness);
        add(pull, [0.0, 0.0, -self.weight * GRAVITY])
    }
}

/// Scanner mirrors in a laser head: they steer where the beam lands across the tool axis,
/// far faster than the arm moves, inside a small window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Galvo {
    /// The largest offset across the tool axis (mm).
    pub range: f64,
    /// The mirrors' position loop (Hz).
    pub bandwidth: f64,
    /// Corrections per second it takes.
    pub hz: f64,
    /// From a correction leaving the planner to the mirrors (s).
    pub latency: f64,
    /// How much of a beam offset moves the deposit: 1 for a weld seam the beam melts where it
    /// lands; under 1 for laser-wire, whose wire stays where the arm holds it (estimate).
    pub capture: f64,
}

/// A tool on the flange.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub kind: ToolKind,
    /// The flange to the tool centre point: z is the tool axis, pointing out of the tool.
    pub tcp: Pose,
    /// Its mass in the flange frame.
    pub body: Body,
    /// Its body's radius about the tool axis (mm), for clearance.
    pub radius: f64,
    pub cable: Option<Cable>,
    pub galvo: Option<Galvo>,
    /// Switching the arm to this head from another (s): a nozzle or focus change on a combined
    /// head, a beam switch between two heads on one flange.
    pub switch: f64,
    /// What it costs with its share of the source (USD, estimate).
    pub cost: f64,
}

/// Steel's density (kg/mm³).
pub const STEEL_DENSITY: f64 = 7.85e-6;

impl Tool {
    /// What a laser-wire head lays (mm³/s of steel).
    pub fn wire_rate(&self) -> Option<f64> {
        match self.kind {
            ToolKind::LaserWire { rate, .. } => Some(rate / 3600.0 / STEEL_DENSITY),
            _ => None,
        }
    }
    pub fn deposits(&self) -> bool {
        matches!(self.kind, ToolKind::LaserWire { .. } | ToolKind::Extruder(_))
    }
    pub fn extruder(&self) -> Option<Extruder> {
        match self.kind {
            ToolKind::Extruder(e) => Some(e),
            _ => None,
        }
    }
    pub fn removes(&self) -> bool {
        matches!(self.kind, ToolKind::LaserCut { .. } | ToolKind::Spindle { .. } | ToolKind::Ablation(_))
    }
    pub fn scans(&self) -> bool {
        matches!(self.kind, ToolKind::Scanner { .. })
    }

    /// How far its axis may lean off the one its path asks for with the process still as
    /// planned (rad; estimates): a weld head 20° (push and drag torch angles in that range are
    /// normal welding practice), a line scanner's incidence 45° (45-60° in the note); none for
    /// the rest (a cut's beam is the route's choice, a nozzle's bead smears). The bead model
    /// takes no torch angle: a leaned bead is planned and laid at the level bead's shape.
    pub fn lean_window(&self) -> f64 {
        match self.kind {
            ToolKind::LaserWire { .. } => 20f64.to_radians(),
            ToolKind::Scanner { .. } => 45f64.to_radians(),
            _ => 0.0,
        }
    }
}

/// Most heads one flange carries.
pub const MAX_HEADS: usize = 4;

/// One arm.
#[derive(Clone, Debug, PartialEq)]
pub struct ArmModel {
    pub name: String,
    /// The cell frame to the arm's base.
    pub base: Pose,
    pub joints: Vec<Joint>,
    /// The links each joint moves, in the joint's frame (link `i` turns with joint `i`).
    pub links: Vec<Body>,
    /// Capsule radius of each link (mm), for clearance.
    pub radius: Q,
    /// The last link's frame to the flange.
    pub flange: Pose,
    /// What the flange carries (at most [`MAX_HEADS`]): one head, a head that works in more than
    /// one mode (one entry per mode, the later ones massless), or more heads side by side.
    pub heads: Vec<Tool>,
    /// The head at work: the TCP, the working end the clearance check leaves out.
    pub head: usize,
    /// What the tool holds or carries beyond itself (flange frame): a held part, a spool.
    pub payload: Body,
    pub controller: Controller,
    /// The links with the tool and payload joined into the last one: what dynamics reads
    /// ([`ArmModel::refresh`] after any change).
    pub bodies: [Body; DOF],
}

impl ArmModel {
    /// Rebuild [`Self::bodies`] after changing a link, a head or the payload.
    pub fn refresh(&mut self) {
        let mut b: [Body; DOF] = std::array::from_fn(|i| self.links.get(i).copied().unwrap_or(Body::NONE));
        for h in &self.heads {
            b[DOF - 1] = b[DOF - 1].joined(&h.body, &self.flange);
        }
        b[DOF - 1] = b[DOF - 1].joined(&self.payload, &self.flange);
        self.bodies = b;
    }

    /// The head at work.
    pub fn tool(&self) -> &Tool {
        &self.heads[self.head]
    }

    /// The heads' whole mass (kg).
    pub fn heads_mass(&self) -> f64 {
        self.heads.iter().map(|h| h.body.mass).sum()
    }

    /// One head on the flange.
    pub fn with_tool(self, tool: Tool) -> ArmModel {
        self.with_heads(vec![tool])
    }

    pub fn with_heads(mut self, heads: Vec<Tool>) -> ArmModel {
        assert!(!heads.is_empty() && heads.len() <= MAX_HEADS, "one to {MAX_HEADS} heads");
        self.heads = heads;
        self.head = 0;
        self.refresh();
        self
    }

    /// Put head `i` to work (the bodies do not change: every head rides along).
    pub fn set_head(&mut self, i: usize) {
        self.head = i.min(self.heads.len() - 1);
    }

    pub fn with_base(mut self, base: Pose) -> ArmModel {
        self.base = base;
        self
    }

    /// The arm's whole moving mass with its tool and payload (kg).
    pub fn moving_mass(&self) -> f64 {
        self.bodies.iter().map(|b| b.mass).sum()
    }

    /// The joint limits as arrays.
    pub fn limits(&self) -> (Q, Q) {
        (std::array::from_fn(|i| self.joints[i].lo), std::array::from_fn(|i| self.joints[i].hi))
    }

    /// The reach from the second joint to the TCP at full stretch (mm): the sum of the
    /// distances between the joint centres after it.
    pub fn reach(&self) -> f64 {
        let mut r = 0.0;
        for j in self.joints.iter().skip(2) {
            r += len(j.origin.t);
        }
        r + len(self.flange.t) + len(self.tool().tcp.t)
    }

    /// A model from a standard DH table: per joint (a, alpha, d, theta offset), mm and rad, with
    /// each link's body given in its DH frame. Joint `i` turns about the z axis of frame `i - 1`;
    /// `template` gives the joints' limits and drives and everything else.
    pub fn from_dh(name: &str, dh: &[(f64, f64, f64, f64); DOF], bodies: &[Body; DOF], template: &ArmModel) -> ArmModel {
        let mut m = template.clone();
        m.name = name.to_string();
        m.links = Vec::with_capacity(DOF);
        // Frame i-1 -> joint i turns about z; then Tz(d) Tx(a) Rx(alpha) lead to DH frame i. The
        // joint frame is frame i-1 turned by the theta offset; the fixed step to frame i goes into
        // the next joint's origin, and each body moves from frame i into its joint frame.
        let mut after = Pose::IDENTITY;
        for (i, &(a, alpha, d, theta)) in dh.iter().enumerate() {
            m.joints[i].origin = after.mul(&Pose::new(rot_z(theta), ZERO));
            m.joints[i].axis = [0.0, 0.0, 1.0];
            after = Pose::new(I3, [0.0, 0.0, d]).mul(&Pose::new(I3, [a, 0.0, 0.0])).mul(&Pose::new(rot_x(alpha), ZERO));
            m.links.push(Body::NONE.joined(&bodies[i], &after));
        }
        m.flange = after;
        m.refresh();
        m
    }
}

/// An arm in a cell: its index in [`crate::io::CellInfo::arms`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ArmId(pub u8);

impl ArmId {
    /// In a two-arm cell: the depositing arm and the trimming one (or the scanning one).
    pub const W: ArmId = ArmId(0);
    pub const C: ArmId = ArmId(1);
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

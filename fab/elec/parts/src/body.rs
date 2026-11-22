//! 3D bodies and their ports: what a case, a panel or a fit check needs from a part (E1 5).

use fab_elec_core::{canon_enum, canon_struct, canon_tags, Angle, Nm, PolySet, Xform3, P};

#[derive(Clone, PartialEq, Debug)]
pub struct Body3d {
    pub source: BodySource,
    /// The body's pose relative to the footprint origin (Z up from the board surface).
    pub offset: Xform3,
    /// Height above the board surface (for keepouts, bodies in stacks, the simple box).
    pub height: Nm,
    pub ports: Vec<BodyPort>,
    /// The author's surface for the whole body, overriding a mesh file's materials.
    pub surface: Option<BodySurface>,
    /// A mesh the author named that the project does not hold yet, by its project path: meanwhile
    /// the body is the part's own (or the courtyard extruded to `height`), and the design lists
    /// the path among the models the project lacks.
    pub missing_mesh: Option<String>,
}
canon_struct!(Body3d { source, offset, height, ports, surface, missing_mesh });

/// A body's stated material and optional finish and colour, with table ids stored as text.
#[derive(Clone, PartialEq, Debug)]
pub struct BodySurface {
    pub material: String,
    /// None takes the material's default finish.
    pub finish: Option<String>,
    /// None takes the material's natural colour.
    pub colour: Option<BodyColour>,
}
canon_struct!(BodySurface { material, finish, colour });

/// A stated body colour, including a swatch's translucency and standard colour code.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BodyColour {
    pub srgb: [f32; 3],
    pub translucency: f32,
    pub ral: Option<u16>,
}
canon_struct!(BodyColour { srgb, translucency, ral });

#[derive(Clone, PartialEq, Debug)]
pub enum BodySource {
    /// An object script of the part library, by its binding.
    Script(String),
    /// A mesh file of the part library, by its library path.
    Mesh(String),
    /// The courtyard extruded to `height`.
    Extrude,
    /// A package family's generated body, sized from the footprint and `height`.
    Package(PackageKind),
}
canon_enum!(BodySource { 0 => Script(s), 1 => Mesh(s), 2 => Extrude, 3 => Package(k) });

/// The package families whose bodies are generated from the footprint.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum PackageKind {
    /// Quad flat no-lead: a moulded body over terminals on four sides.
    Qfn,
    /// Small-outline gull-wing: a moulded body with leads that bend out of its sides and down
    /// onto their pads (SOIC, TSSOP, SOT-23, and quad versions).
    Sop,
    /// Two-terminal chip: a ceramic block between metal end caps (chip resistors, capacitors,
    /// inductors and fuses).
    Chip,
    /// Display module: a glass cover with a black bezel over a dark active area, on a metal back,
    /// with a flex to its contacts on the board.
    Display,
}
canon_tags!(PackageKind);

impl PackageKind {
    /// The family by its script name (`package: @qfn`).
    pub fn lookup(name: &str) -> Option<PackageKind> {
        match name {
            "qfn" => Some(PackageKind::Qfn),
            "sop" => Some(PackageKind::Sop),
            "chip" => Some(PackageKind::Chip),
            "display" => Some(PackageKind::Display),
            _ => None,
        }
    }
}

/// What reaches through a case at a port.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum BodyPortKind {
    /// A mating plug goes in (connectors).
    Plug,
    /// Light comes out (LEDs, displays).
    Light,
    /// A finger or actuator presses (buttons, switches).
    Press,
    /// Sound or air passes (buzzers, microphones, fans).
    Air,
    /// A cable leaves (glands, strain reliefs).
    Cable,
    /// A shaft stands out of the body for a knob (potentiometers, encoders): `at` is its tip,
    /// `stand` how far it stands out of the housing, the profile its section.
    Shaft,
}
canon_tags!(BodyPortKind);

/// A rotary part's turn from one end of its travel to the other, the common 300 degrees: a knob or
/// shaft shows its part's place as that share of it, at mid travel as drawn, and turns clockwise
/// (seen down the shaft from its tip) as the place rises.
pub const SHAFT_TURN_DEG: f64 = 300.0;

/// A port of a body: a profile in the plane normal to `dir` at `at`, reaching `reach` along it.
#[derive(Clone, PartialEq, Debug)]
pub struct BodyPort {
    pub kind: BodyPortKind,
    pub profile: PolySet,
    /// Port origin in body coordinates (nm).
    pub at: [Nm; 3],
    /// Unit direction out of the body.
    pub dir: [f64; 3],
    /// How far the opening must extend along `dir` (a plug's insertion depth, a light pipe).
    pub reach: Nm,
    /// Clearance a case keeps around the profile.
    pub margin: Nm,
    /// A press port's actuation travel: how far its plunger moves down to switch, which whatever
    /// presses it through a case must be able to follow. 0 where the part states none.
    pub travel: Nm,
    /// How far a press port's plunger, or a shaft port's shaft, stands out of its housing along
    /// `dir`: the housing's face is `stand` behind `at`. 0 where the part states none.
    pub stand: Nm,
}
canon_struct!(BodyPort { kind, profile, at, dir, reach, margin, travel, stand });

impl BodyPort {
    /// The same port in another body frame: `to` maps this port's frame into the other. Its mouth,
    /// its direction and its profile (on the new direction's [`port_axes`]) keep their place.
    pub fn moved(&self, to: &Xform3) -> BodyPort {
        let dir = to.rotate(self.dir);
        let (u_from, v_from) = port_axes(self.dir);
        let (u, v) = port_axes(dir);
        let rings: Vec<Vec<P>> = self.profile.rings().map(|ring| ring.iter().map(|p| {
            let offset = to.rotate(std::array::from_fn(|i| p.x as f64 * u_from[i] + p.y as f64 * v_from[i]));
            P::from_f(dot(offset, u), dot(offset, v))
        }).collect()).collect();
        BodyPort { profile: PolySet::from_rings_even_odd(&rings), at: to.apply(self.at), dir, ..self.clone() }
    }
}

/// The axes a port's profile is drawn on, square to its direction: `v` is +z made square to `dir`
/// (+y when `dir` is along z) and `u` = `v` x `dir`. The cross products give the normalised
/// projection of +z without subtracting nearly equal vectors; a direction along z (including
/// rotation roundoff) takes +y as its up reference.
pub fn port_axes(dir: [f64; 3]) -> ([f64; 3], [f64; 3]) {
    let dir = normalised(dir);
    let mut u = cross([0.0, 0.0, 1.0], dir);
    if dot(u, u) <= 1e-24 {
        u = cross([0.0, 1.0, 0.0], dir);
    }
    let u = normalised(u);
    (u, cross(dir, u))
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn normalised(v: [f64; 3]) -> [f64; 3] {
    let len = dot(v, v).sqrt();
    v.map(|x| x / len)
}

/// A light source's emitting faces, in the part's frame (E7 1.3): the face of its body's @light
/// port (`at`, `dir`, `size`, `angle`), and any further `faces` (a chip LED's dome walls), each with
/// its share of the part's flux. The port's face keeps the rest, all of it when there are none.
#[derive(Clone, PartialEq, Debug)]
pub struct Emitter {
    /// Centre of the face and its outward normal.
    pub at: [Nm; 3],
    pub dir: [f64; 3],
    /// Width and height of the face.
    pub size: P,
    /// The port's face's full viewing angle at half intensity.
    pub angle: Angle,
    pub light: Light,
    /// Luminous flux per ampere, in lumens.
    pub flux_per_a: Option<f64>,
    /// The faces beside the port's, each with its share of the flux.
    pub faces: Vec<EmitterFace>,
}
// Additive: `faces` is written only when there are some, so a one-face emitter keeps the hash it had.
impl fab_elec_core::canon::Canon for Emitter {
    fn canon(&self, w: &mut fab_elec_core::canon::Writer) {
        let Emitter { at, dir, size, angle, light, flux_per_a, faces } = self;
        at.canon(w);
        dir.canon(w);
        size.canon(w);
        angle.canon(w);
        light.canon(w);
        flux_per_a.canon(w);
        if !faces.is_empty() {
            faces.canon(w);
        }
    }
}

/// One further emitting face of a light source, in the part's frame.
#[derive(Clone, PartialEq, Debug)]
pub struct EmitterFace {
    /// Centre of the face and its outward unit normal.
    pub at: [Nm; 3],
    pub dir: [f64; 3],
    /// Width and height of the face.
    pub size: P,
    /// Full viewing angle at half intensity (120 degrees for a Lambertian face).
    pub angle: Angle,
    /// Its share of the part's flux, 0 to 1.
    pub share: f64,
}
canon_struct!(EmitterFace { at, dir, size, angle, share });

impl Emitter {
    /// The port's face's share of the part's flux: what the further faces leave.
    pub fn port_share(&self) -> f64 {
        1.0 - self.faces.iter().map(|face| face.share).sum::<f64>()
    }

    /// Every emitting face with its share, the port's first. O(faces).
    pub fn lobes(&self) -> impl Iterator<Item = EmitterFace> + '_ {
        let port = EmitterFace { at: self.at, dir: self.dir, size: self.size, angle: self.angle, share: self.port_share() };
        std::iter::once(port).chain(self.faces.iter().cloned())
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum Light {
    /// Linear RGB.
    Rgb([f32; 3]),
    /// Peak wavelength in nanometres.
    Peak(f64),
    /// (wavelength nm, relative power) samples.
    Spectrum(Vec<(f64, f64)>),
}
canon_enum!(Light { 0 => Rgb(c), 1 => Peak(w), 2 => Spectrum(s) });

impl Light {
    /// A generic indicator LED's light by colour name (the names `e.led` takes): the die's typical
    /// peak wavelength, white as linear RGB; `None` for a name with no generic die.
    pub fn of_led_color(color: &str) -> Option<Light> {
        Some(match color {
            "red" => Light::Peak(630.0),
            "orange" => Light::Peak(610.0),
            "yellow" => Light::Peak(590.0),
            // A true green die; the older yellow-green die is its own name.
            "green" => Light::Peak(525.0),
            "yellow-green" => Light::Peak(570.0),
            "blue" => Light::Peak(470.0),
            "infrared" => Light::Peak(940.0),
            "white" => Light::Rgb([1.0, 1.0, 1.0]),
            _ => return None,
        })
    }
}

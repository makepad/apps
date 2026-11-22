//! Fab's materials, surface finishes and standard colours, shared by electronics, cases, moulds and
//! printing.
//!
//! - A [`Material`] is a resin, elastomer or glass: density, optics (index, Abbe number, clarity),
//!   moulding rules (shrinkage, walls, draft, snap strain), mechanics, heat, electrical class and its
//!   rendering defaults.
//! - A [`Finish`] is a surface finish by its public standard or process (polish grades, spark-erosion
//!   grades, grain depths, print layers, machined). One finish id drives four things: the render
//!   roughness and procedural texture, the tracer's surface roughness, the draft the process needs
//!   for that texture, and the note on the drawing.
//! - A [`Colour`] is an sRGB colour with its translucency, optionally a standard colour code.
//! - A [`Surface`] is what one face of a part is: material, finish, colour. A part's faces index a
//!   table of them; the [`Medium`] of a clear surface is what the tracer reads.
//!
//! Values are typical figures for unfilled general-purpose grades, rounded mid-range values from
//! public datasheets and design guides. Real grades vary by 20 % or more: use them for previews,
//! checks and first-pass design rules, never as a datasheet.
//!
//! Modules: [`material`] and [`finish`] hold the tables and the rules read from them, [`ral`] the
//! standard colour collection, [`swatch`] the generic named colours. [`lookup`] resolves any name a
//! script writes (`@pmma`, `@vdi_27`, `"RAL 9005"`, `"smoke"`).

pub mod finish;
pub mod material;
pub mod part_look;
pub mod ral;
pub mod shade;
pub mod swatch;

/// What a name resolves to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Lookup {
    Material(MaterialId),
    Finish(FinishId),
    Colour(Colour),
}

/// Any name a script writes: a material, then a finish, then a swatch or standard colour.
pub fn lookup(name: &str) -> Option<Lookup> {
    MaterialId::lookup(name)
        .map(Lookup::Material)
        .or_else(|| FinishId::lookup(name).map(Lookup::Finish))
        .or_else(|| swatch::colour_by_name(name).map(Lookup::Colour))
}

/// A value range; `0..0` means "not applicable".
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Range {
    pub lo: f32,
    pub hi: f32,
}

impl Range {
    pub const fn new(lo: f32, hi: f32) -> Self {
        Self { lo, hi }
    }
    pub fn mid(&self) -> f32 {
        0.5 * (self.lo + self.hi)
    }
    pub fn is_empty(&self) -> bool {
        self.lo == 0.0 && self.hi == 0.0
    }
}

/// An sRGB colour, gamma encoded, 0..1 per channel.
pub type Srgb = [f32; 3];

/// The processes a material or finish applies to, as a bit set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Processes(pub u8);

impl Processes {
    pub const NONE: Self = Self(0);
    pub const INJECTION: Self = Self(1 << 0);
    pub const FDM: Self = Self(1 << 1);
    pub const SLA: Self = Self(1 << 2);
    pub const CNC: Self = Self(1 << 3);
    /// Powder bed fusion.
    pub const SLS: Self = Self(1 << 4);

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

impl std::ops::BitOr for Processes {
    type Output = Self;
    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

/// The family a material belongs to. `Glass` holds optical references for renders (glasses, crystal,
/// diamond); they are not moulded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Commodity,
    Engineering,
    HighPerformance,
    Elastomer,
    Print3d,
    Glass,
    /// Metals and metal finishes (copper and its platings, contacts, shells, cans): render rows, `f0` set.
    Metal,
    /// Thin coatings over another surface (solder mask, legend ink, resistor top coat).
    Coating,
    /// Circuit-board cores and films (woven glass in epoxy, polyimide, high-frequency laminates).
    Laminate,
    /// Ceramic and ferrite component bodies.
    Ceramic,
    /// Component and cable body materials described for rendering only (moulding compounds, lenses,
    /// jackets): no moulding rules.
    Body,
}

impl Family {
    /// A plastic or glass of the moulding tables, whose design-rule fields are filled.
    pub const fn is_moulding(self) -> bool {
        matches!(self, Family::Commodity | Family::Engineering | Family::HighPerformance | Family::Elastomer | Family::Print3d | Family::Glass)
    }
}

/// The best optical grade commonly available for the polymer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Clarity {
    Clear,
    Translucent,
    Opaque,
}

/// The load the heat deflection temperature was measured at.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HdtLoad {
    None,
    MPa045,
    MPa18,
}

/// How a material demoulds; it picks the draft column of the finish table.
/// - `Easy`: semi-crystalline, low friction or flexible (POM, PA, PBT, PP, PE, elastomers).
/// - `Standard`: amorphous styrenics and blends (ABS, ASA, HIPS, PC/ABS, PVC).
/// - `Stiff`: rigid amorphous or filled resins that grip the core or scuff (PC, PMMA, PS, PETG,
///   PEEK, PPS, glass filled).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DraftClass {
    Easy,
    Standard,
    Stiff,
}

impl DraftClass {
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Electrostatic classes by surface resistance; EMI shielding is a further step down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ElectricalClass {
    Insulating,
    StaticDissipative,
    Conductive,
    EmiShielding,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Electrical {
    /// Ohm cm.
    pub volume_resistivity: Range,
    /// Ohm per square.
    pub surface_resistivity: Range,
    pub class: ElectricalClass,
    /// Carbon filled: black or dark grey only, no translucency, no light colours.
    pub dark_only: bool,
}

/// What a material does to light: the tracer's dielectric bulk and the realtime view's refraction.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Optics {
    /// nD of the clear or natural grade; 0 when no transparent grade is common.
    pub ior: f32,
    /// Abbe number Vd = (nd - 1) / (nF - nC); 0 exactly when `ior` is 0. Low disperses strongly
    /// (PC about 30), high weakly (PMMA about 57).
    pub abbe: f32,
    pub clarity: Clarity,
    /// Mean free path of bulk scattering in the natural compound, mm; 0 when the bulk is clear (a
    /// pigmented part's scattering then follows its colour's translucency).
    pub scatter_mm: f32,
}

/// A material: a resin, an elastomer or an optical glass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Material {
    /// The id in files and scripts: `pc_abs`.
    pub id: &'static str,
    /// `PC/ABS`.
    pub short_name: &'static str,
    pub full_name: &'static str,
    pub family: Family,
    pub processes: Processes,
    /// g/cm^3.
    pub density: f32,
    pub optics: Optics,
    /// %, linear mould shrinkage; empty when not moulded.
    pub shrinkage: Range,
    /// mm, recommended nominal wall.
    pub wall: Range,
    /// mm, practical minimum wall.
    pub min_wall: f32,
    /// Degrees per side on polished faces.
    pub draft_polished: f32,
    /// Degrees per side on lightly textured faces; deeper textures: [`Finish::draft_deg`].
    pub draft_textured: f32,
    pub draft_class: DraftClass,
    /// Takes a mirror polish faithfully.
    pub holds_high_gloss: bool,
    /// %, allowable short-term strain for a one-time snap fit.
    pub snap_strain: f32,
    /// GPa, tensile.
    pub modulus: f32,
    /// MPa, yield or break, whichever comes first.
    pub tensile_strength: f32,
    /// kJ/m^2, notched Charpy at 23 C; 0 when no break or not meaningful.
    pub notched_impact: f32,
    /// C, heat deflection temperature at `hdt_load`.
    pub hdt: f32,
    pub hdt_load: HdtLoad,
    /// C, continuous with no load.
    pub max_service_temp: f32,
    /// C, processing (melt for moulding, nozzle for printing); empty when not applicable.
    pub melt_temp: Range,
    pub electrical: Electrical,
    /// GGX perceptual roughness of an as-moulded part, 0..1.
    pub roughness: f32,
    /// The colour of the unpigmented resin.
    pub natural: Srgb,
    /// Translucent or clear tints are possible.
    pub can_translucent: bool,
    /// How translucent the natural compound is (0 opaque, 1 clear): a laminate's or a lens's own look.
    /// 0 for the moulding plastics, whose translucency is the pigmented colour's.
    pub translucency: f32,
    /// A metal's linear reflectance at normal incidence (what a shader uses directly); `None` for
    /// dielectrics, whose reflectance follows from the index.
    pub f0: Option<[f32; 3]>,
    /// A coating's typical thickness in micrometres (over copper, over laminate for a mask); empty
    /// when the material is no coating.
    pub coat_um: Range,
    /// Typical uses and caveats, one line.
    pub notes: &'static str,
    /// Processing hints, one line.
    pub processing: &'static str,
    /// 1 (cheapest) to 5, relative price per kg.
    pub cost_tier: u8,
}

/// The standard or process a finish belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FinishStandard {
    /// Mould polish grades A-1 to D-3.
    Spi,
    /// Spark-erosion texture references 12 to 45.
    Vdi3400,
    /// Etched grain by depth.
    Texture,
    /// As printed, sanded, smoothed.
    Print,
    /// As machined.
    Machined,
    /// A coating's own gloss grade (solder mask), whatever it lies on.
    Coating,
    /// The material's own surface as made, plated or fired: its roughness is the material's.
    AsMade,
}

/// A surface finish.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Finish {
    /// The id in files and scripts: `spi_a2`, `vdi_27`, `fdm_0_2`.
    pub id: &'static str,
    pub standard: FinishStandard,
    /// `A-2`, `27`, ...
    pub grade: &'static str,
    /// How it is made: "Grade 6 diamond buff, high gloss".
    pub description: &'static str,
    /// Micrometres.
    pub ra_um: Range,
    /// Micrometres, typical; 0 when not published.
    pub rz_um: f32,
    pub processes: Processes,
    /// Keeps a clear part optically clear (lenses, light-pipe faces).
    pub clear_ok: bool,
    /// A clear or translucent part looks diffusing.
    pub frosted: bool,
    /// Minimum degrees of draft per side, by [`DraftClass`]; 0 for prints and machined finishes.
    pub draft_deg: [f32; 3],
}

/// The visible texture of a finish, for procedural bump shading (realtime and tracer). Polished
/// grades and smoothed prints are `None`: their structure is far below a pixel and the roughness
/// covers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextureKind {
    #[default]
    None,
    Edm,
    Blast,
    Grain,
    FdmLayers,
    SlaLayers,
    Machined,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SurfaceTexture {
    pub kind: TextureKind,
    /// Peak to valley, mm.
    pub depth_mm: f32,
    /// Feature spacing, layer height or tool pitch, mm.
    pub size_mm: f32,
}

/// A material in the table. Files and scripts name it by [`Material::id`]; the index is stable
/// within a build only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MaterialId(pub u16);

/// A finish in the table. Files and scripts name it by [`Finish::id`]; the index is stable within
/// a build only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FinishId(pub u16);

impl MaterialId {
    /// By id, short name or full name, case-insensitive, `-` and `_` alike, or a common name
    /// (`pmma_clear`). Trade names are not materials and are refused.
    pub fn lookup(name: &str) -> Option<Self> {
        material::lookup(name)
    }
    pub fn get(self) -> &'static Material {
        &material::MATERIALS[self.0 as usize]
    }
    pub fn name(self) -> &'static str {
        self.get().id
    }
    pub fn all() -> impl Iterator<Item = Self> {
        (0..material::MATERIALS.len() as u16).map(Self)
    }
}

impl FinishId {
    /// By id or label, case-insensitive (`spi_a2`, `SPI A-2`, `a2`, `VDI 27`, `VDI 3400 Ref 27`,
    /// `fdm-0.2`), or a common name (`polished`, `frosted`, `textured`).
    pub fn lookup(name: &str) -> Option<Self> {
        finish::lookup(name)
    }
    pub fn get(self) -> &'static Finish {
        &finish::FINISHES[self.0 as usize]
    }
    pub fn name(self) -> &'static str {
        self.get().id
    }
    pub fn all() -> impl Iterator<Item = Self> {
        (0..finish::FINISHES.len() as u16).map(Self)
    }
}

/// A colour as a part is made in it: sRGB, how translucent the compound is (0 opaque, 1 clear), and
/// the standard colour code it was picked from, for drawings and orders.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Colour {
    pub srgb: Srgb,
    pub translucency: f32,
    /// A standard colour code (`9005`), when the colour is one.
    pub ral: Option<u16>,
}

impl Colour {
    pub const fn srgb(srgb: Srgb) -> Self {
        Self { srgb, translucency: 0.0, ral: None }
    }
    /// `0xRRGGBB`.
    pub const fn hex(v: u32) -> Self {
        Self::srgb([
            ((v >> 16) & 255) as f32 / 255.0,
            ((v >> 8) & 255) as f32 / 255.0,
            (v & 255) as f32 / 255.0,
        ])
    }
    pub const fn with_translucency(self, translucency: f32) -> Self {
        Self { translucency, ..self }
    }
}

/// What one face of a part is: its material, finish and colour. A part carries a table of these and
/// every face (triangle, B-rep face, field material slot) indexes it, so a finish survives booleans.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Surface {
    pub material: MaterialId,
    pub finish: FinishId,
    pub colour: Colour,
}

impl Surface {
    /// A material in its default finish and natural colour, as translucent as the natural compound;
    /// clear grades are clear (translucency 0.85, the clear swatch's).
    pub fn new(material: MaterialId) -> Self {
        let m = material.get();
        let clear = m.optics.clarity == Clarity::Clear && m.translucency_allowed();
        let translucency = if m.translucency > 0.0 {
            m.translucency
        } else if clear {
            0.85
        } else {
            0.0
        };
        Self { material, finish: m.default_finish(), colour: Colour::srgb(m.natural).with_translucency(translucency) }
    }

    /// The bulk and face the tracer reads: the material's index and dispersion, tinted by the
    /// colour over 40 mm of path. The bulk scatters at the material's own mean free path when it has
    /// one (laminates, mask), else compounds less translucent than 0.7 scatter, the less translucent
    /// the shorter the path.
    pub fn medium(&self) -> Medium {
        let (m, f) = (self.material.get(), self.finish.get());
        let lin = self.colour.srgb.map(fab_colour::oklab::to_linear);
        let max = lin[0].max(lin[1]).max(lin[2]);
        let tint = if max > 0.0 { lin.map(|v| v / max) } else { [1.0; 3] };
        let t = self.colour.translucency;
        let (scatter_distance_mm, scatter_anisotropy) = if m.optics.scatter_mm > 0.0 {
            (m.optics.scatter_mm, 0.7)
        } else if t < 0.7 {
            (0.4 + 6.0 * t.max(0.0), 0.7)
        } else {
            (0.0, 0.0)
        };
        let clear = m.optics.ior > 0.0;
        Medium {
            ior: if clear { m.optics.ior } else { 1.5 },
            abbe: if clear { m.optics.abbe } else { 0.0 },
            tint,
            tint_distance_mm: 40.0,
            scatter_distance_mm,
            scatter_anisotropy,
            roughness: if f.standard == FinishStandard::AsMade { m.roughness } else { f.render_roughness() },
            f0: m.f0,
        }
    }
}

/// The bulk and face of a surface as the tracer reads it: a dielectric's index, tint and scatter,
/// or a metal's reflectance.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Medium {
    pub ior: f32,
    pub abbe: f32,
    /// Linear transmittance after `tint_distance_mm` of path (Beer-Lambert), brightest channel 1.
    pub tint: [f32; 3],
    pub tint_distance_mm: f32,
    /// Mean free path of bulk scattering, mm; 0 for none.
    pub scatter_distance_mm: f32,
    /// Henyey-Greenstein g, -1..1.
    pub scatter_anisotropy: f32,
    /// GGX perceptual roughness of the face: its finish's, or the material's own when the finish
    /// is as made.
    pub roughness: f32,
    /// A metal's linear reflectance at normal incidence; `None` for a dielectric.
    pub f0: Option<[f32; 3]>,
}

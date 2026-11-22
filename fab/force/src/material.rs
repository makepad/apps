//! The printed-part material model: the stiffness and strength of a part as it will print,
//! not as solid plastic.
//!
//! # The model
//!
//! An FDM part is three materials, each orthotropic, in the printer frame (Z the build
//! direction):
//!
//! - **Walls**: the perimeters, beads running along the outline, `wall_loops` of them
//!   (thickness `outer + (loops - 1) * inner spacing`) inside every outer surface whose normal
//!   has a horizontal part. Axis 1 is the bead (the outline's tangent), 2 across the beads in
//!   the layer, 3 the build direction.
//! - **Skin**: the top and bottom solid layers (`top_layers` / `bottom_layers` times the layer
//!   height under an upward / over a downward facing surface): beads at the infill direction,
//!   turning 90 degrees every layer, so the skin is the average of the two plies' stiffness.
//! - **Infill**: the sparse pattern at its density, homogenised from the same plies by the
//!   pattern's scaling law (below), in the frame of the infill direction.
//!
//! ## The bead (one ply)
//!
//! A filament family gives the polymer's moduli along and across a bead (`e_long`,
//! `e_trans`; equal for an unfilled polymer, a fibre fill makes the bead stiffer along its
//! length), Poisson's ratio, and the strengths a printed part shows along the beads, between
//! the beads of a layer, and between layers (`x_long`, `x_trans`, `x_layer`, at the
//! reference layer height and line width). The beads leave voids: with void fraction `v` the
//! moduli follow the mechanics-of-materials estimate of Rodriguez, Thomas and Renaud
//! ("Mechanical behavior of acrylonitrile butadiene styrene fused deposition materials
//! modeling", Rapid Prototyping Journal 9(4), 2003):
//!
//! ```text
//! E1 = (1 - v) e_long        E2 = (1 - sqrt v) e_trans        E3 = k_layer (1 - sqrt v) e_trans
//! ```
//!
//! `k_layer` is the family's inter-layer bond quality (stiffness between layers relative to
//! across beads). The void fraction scales with the bead's aspect ratio: the voids are the
//! cusps between rounded bead ends, so `v = v_ref (h / w) / (h_ref / w_ref)` with
//! `v_ref = 0.06` at `h/w = 0.2/0.45`, inside the 2-11 % measured for FDM parts (Rodriguez,
//! Thomas and Renaud, "Mechanical behavior of ABS fused deposition materials. Experimental
//! investigation", Rapid Prototyping Journal 7(3), 2001). The shear moduli are
//! `(1 - sqrt v) e_trans / (2 (1 + nu))` in the 12 and 13 planes and `k_layer` times that in
//! the 23 plane.
//!
//! The layer bond's strength scales with the share of a bead's width that touches the layer
//! below: stadium-shaped beads of width `w` and height `h` touch over `w - h`, so
//! `x_layer` is multiplied by `((w - h) / w) / ((w_ref - h_ref) / w_ref)`, clamped to
//! 0.3..1.2 (the geometric bond-width argument of Coogan and Kazmer, "Bond and part strength
//! in fused deposition modeling", Rapid Prototyping Journal 23(2), 2017). Taller layers are
//! weaker between layers; finer ones stronger, up to the bead's own transverse strength.
//!
//! ## Infill
//!
//! Every pattern is a lattice of plies. Its moduli and strengths in each direction follow a
//! scaling law of the density `rho` (Gibson and Ashby, "Cellular Solids", 2nd ed. 1997;
//! Deshpande, Ashby and Fleck, "Foam topology: bending versus stretching dominated
//! architectures", Acta Materialia 49, 2001):
//!
//! ```text
//! X(rho) = X_skin rho^n (C + (1 - C) rho)
//! ```
//!
//! `n = 1` where the lines carry the load along their length (stretching), 2 through
//! contacts or struts, 3 where they bend; `C` is the lattice's coefficient at low density,
//! and the law meets the solid skin at `rho = 1`. Strengths follow the same law with
//! exponent `1/2 + n/2` (stretching 1, struts 1.5, bending 2, as Gibson and Ashby give for
//! the plastic collapse of stretching, strut and bending lattices). [`Infill`] lists each
//! family's `(n, C)` per direction:
//!
//! - lines turning 90 degrees a layer (rectilinear, line, zig-zag): in plane each direction's
//!   lines are half the layers, `(1, e_long / (e_long + e_trans))`; across layers the
//!   load passes through the crossings, a `rho^2` share of the area, `(2, 1)`; in-plane shear
//!   bends the lines, `(3, 1)`;
//! - lines the same way every layer: walls stacked in Z, stiff along the lines and in Z,
//!   bending across;
//! - grids and triangles: vertical walls (`n = 1` in Z); in plane a square grid stretches
//!   along its lines and bends in shear, a triangulated one stretches in every direction
//!   (Gibson and Ashby's triangular honeycomb: `E = rho/3`, `G = rho/8`);
//! - hexagon prisms: in plane the cell walls bend (`E = 2.3 (t/l)^3`, `t/l = sqrt(3)/2 rho`),
//!   along the prisms they stretch;
//! - cubic-type 3D lattices (cubic, adaptive and support cubic, 3D honeycomb, 2D lattice):
//!   tilted plates, stretching in every direction at `rho/3`;
//! - gyroid: a sheet minimal surface, `(1.5, 0.5)` in every direction, between stretching
//!   and bending as measured for polymer sheet lattices (Maskery et al., "Insights into the
//!   mechanical properties of several triply periodic minimal surface lattice structures made
//!   by polymer additive manufacturing", Polymer 152, 2018);
//! - spirals, space-filling curves and concentric loops stacked alike: walls in Z, in plane
//!   without a direction, `(1, 0.4)`;
//! - lightning: holds up the surfaces above it and nothing else (`(1, 0.02)`).
//!
//! The scaled moduli keep the matrix positive definite by scaling the skin's stiffness
//! symmetrically, `D' = L D L` with `L = diag(sqrt k)`: each modulus scales by its `k`,
//! Poisson's ratios by `sqrt(k_i / k_j)`.
//!
//! ## Strength
//!
//! Each material is checked in its own frame with Hill's quadratic criterion for orthotropic
//! materials (R. Hill, "A theory of the yielding and plastic flow of anisotropic metals",
//! Proc. Royal Society A 193, 1948; Azzi and Tsai, "Anisotropic strength of composites",
//! Experimental Mechanics 5, 1965), with the tensile strengths (conservative: printed parts
//! are stronger in compression). For an isotropic material it is von Mises. The index is
//! quadratic in the stress, so the safety factor is `1 / sqrt(index)`. Where the walls, skin
//! and infill mix in a cell, each sees the same strain (the Voigt mixture) and the weakest
//! decides.
//!
//! The filament families' figures are typical values for generic unfilled and fibre-filled
//! filaments of each family, printed at 0.2 mm layers and 0.45 mm lines.
//!
//! ## Zones
//!
//! A part may print with other settings inside volumes of its own ([`Zone`], a script's
//! `m.settings` region). There the infill's density and pattern and the wall count are the
//! zone's, where it sets them; where two overlap, the later one's value of a key wins. The
//! infill inside follows the same law as the part's own, and the walls are as thick as the
//! zone's count makes them.

use crate::element::{mul6, Mat6, V3};
use std::sync::Arc;

/// A filament family's bead.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filament {
    pub name: &'static str,
    /// Moduli along and across a bead (MPa).
    pub e_long: f64,
    pub e_trans: f64,
    pub nu: f64,
    /// Inter-layer stiffness relative to across beads.
    pub k_layer: f64,
    /// Tensile strengths along the beads, between beads in a layer, and between layers (MPa).
    pub x_long: f64,
    pub x_trans: f64,
    pub x_layer: f64,
}

impl Filament {
    /// The strength between layers (MPa) of lines `w` wide and `h` tall: a bead's bond with the layer
    /// below is the share of its width that touches it.
    pub fn x_layer_of(&self, h: f64, w: f64) -> f64 {
        let (w, h) = (w.max(0.05), h.clamp(0.02, w.max(0.05)));
        let bond = (((w - h) / w) / ((W_REF - H_REF) / W_REF)).clamp(0.3, 1.2);
        (self.x_layer * bond).min(self.x_trans.max(self.x_layer))
    }

    /// How many times weaker a print is pulled apart across its layers than along its beads.
    pub fn layer_factor(&self, h: f64, w: f64) -> f64 {
        self.x_long / self.x_layer_of(h, w)
    }

    /// A line's strength (MPa) pulled at `angle` (radians) to the bed, by the Hill criterion in the
    /// bead's plane: the bead's strength at 0, the bond's at a right angle, the shear between them.
    pub fn pull_strength(&self, angle: f64, h: f64, w: f64) -> f64 {
        let (c, z) = (angle.cos().powi(2), angle.sin().powi(2));
        let (x1, x3) = (self.x_long, self.x_layer_of(h, w));
        let s13 = x3 / 3f64.sqrt();
        1.0 / (c * c / (x1 * x1) + z * z / (x3 * x3) + c * z * (1.0 / (s13 * s13) - 1.0 / (x1 * x1))).sqrt()
    }
}

pub const PLA: Filament = Filament { name: "PLA", e_long: 3300.0, e_trans: 3300.0, nu: 0.35, k_layer: 0.9, x_long: 55.0, x_trans: 40.0, x_layer: 32.0 };
pub const PETG: Filament = Filament { name: "PETG", e_long: 2050.0, e_trans: 2050.0, nu: 0.38, k_layer: 0.9, x_long: 48.0, x_trans: 38.0, x_layer: 30.0 };
pub const ABS_ASA: Filament = Filament { name: "ABS/ASA", e_long: 2100.0, e_trans: 2100.0, nu: 0.35, k_layer: 0.85, x_long: 38.0, x_trans: 28.0, x_layer: 22.0 };
pub const PA_CF: Filament = Filament { name: "PA-CF", e_long: 6500.0, e_trans: 2700.0, nu: 0.33, k_layer: 0.8, x_long: 85.0, x_trans: 40.0, x_layer: 30.0 };

/// The presets the Force panel offers.
pub const FILAMENTS: [Filament; 4] = [PLA, PETG, ABS_ASA, PA_CF];

/// The preset for a slicer's filament type (`filament_type`: "PLA", "PETG", "ASA",
/// "PA-CF", ...), if one fits.
pub fn filament_of_type(t: &str) -> Option<&'static Filament> {
    let t = t.to_ascii_uppercase();
    let fibre = t.contains("CF") || t.contains("GF");
    if fibre && (t.starts_with("PA") || t.starts_with("PPA")) {
        return Some(&PA_CF);
    }
    if t.starts_with("PLA") {
        return Some(&PLA);
    }
    if t.starts_with("PET") {
        return Some(&PETG);
    }
    if t.starts_with("ABS") || t.starts_with("ASA") {
        return Some(&ABS_ASA);
    }
    None
}

/// The reference bead the families' figures are for (mm).
const H_REF: f64 = 0.2;
const W_REF: f64 = 0.45;
const V_REF: f64 = 0.06;

/// Infill families: patterns that carry load alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Infill {
    /// One line family per layer, turning 90 degrees every layer.
    Alternating,
    /// One line family, the same direction every layer.
    Aligned,
    /// Two crossing families per layer: square cells, walls in Z.
    Grid,
    /// Triangulated in plane, walls in Z.
    Triangles,
    /// Hexagon prisms.
    Honeycomb,
    /// Tilted plates in 3D (cubic and its kin).
    Cubic,
    /// A gyroid sheet.
    Gyroid,
    /// Curves stacked alike every layer (spirals, space-filling curves, concentric loops).
    Curves,
    /// Only holds up the surfaces above it.
    Lightning,
}

impl Infill {
    /// The family of a slicer's sparse infill pattern, by the setting's value
    /// (`sparse_infill_pattern`). Patterns whose density varies (adaptive and support cubic)
    /// take their family's law at the nominal density, which overrates them away from the
    /// surfaces they thicken towards. None: a pattern this model does not know.
    pub fn from_setting(name: &str) -> Option<Infill> {
        Some(match name {
            "zig-zag" | "line" | "zigzag" | "crosszag" | "lockedzag" => Infill::Alternating,
            "alignedrectilinear" => Infill::Aligned,
            "grid" | "crosshatch" => Infill::Grid,
            "triangles" | "tri-hexagon" => Infill::Triangles,
            "honeycomb" => Infill::Honeycomb,
            "cubic" | "adaptivecubic" | "supportcubic" | "2dlattice" | "3dhoneycomb" => Infill::Cubic,
            "gyroid" => Infill::Gyroid,
            "lightning" => Infill::Lightning,
            "hilbertcurve" | "archimedeanchords" | "octagramspiral" | "concentric" => Infill::Curves,
            _ => return None,
        })
    }

    /// `(n, C)` of the scaling law per direction: x, y, z (normal), then the yz, xz, xy shears.
    pub fn laws(self, f: &Filament) -> [(f64, f64); 6] {
        let half = f.e_long / (f.e_long + f.e_trans);
        match self {
            Infill::Alternating => [(1.0, half), (1.0, half), (2.0, 1.0), (2.0, 1.0), (2.0, 1.0), (3.0, 1.0)],
            Infill::Aligned => [(1.0, 1.0), (3.0, 1.0), (1.0, 1.0), (3.0, 1.0), (1.0, 1.0), (3.0, 1.0)],
            Infill::Grid => [(1.0, 0.5), (1.0, 0.5), (1.0, 1.0), (1.0, 0.5), (1.0, 0.5), (3.0, 1.0)],
            Infill::Triangles => [(1.0, 1.0 / 3.0), (1.0, 1.0 / 3.0), (1.0, 1.0), (1.0, 0.5), (1.0, 0.5), (1.0, 0.34)],
            Infill::Honeycomb => [(3.0, 1.0), (3.0, 1.0), (1.0, 1.0), (1.0, 0.6), (1.0, 0.6), (3.0, 1.0)],
            Infill::Cubic => [(1.0, 1.0 / 3.0); 6],
            Infill::Gyroid => [(1.5, 0.5); 6],
            Infill::Curves => [(1.0, 0.4), (1.0, 0.4), (1.0, 1.0), (1.0, 0.4), (1.0, 0.4), (3.0, 1.0)],
            Infill::Lightning => [(1.0, 0.02); 6],
        }
    }
}

/// The print settings the model reads (mm, degrees, 0..1).
#[derive(Clone, Debug, PartialEq)]
pub struct Print {
    pub layer_height: f64,
    pub outer_width: f64,
    pub inner_width: f64,
    pub wall_loops: usize,
    pub top_layers: usize,
    pub bottom_layers: usize,
    pub density: f64,
    pub infill: Infill,
    /// The infill (and solid layers') direction, degrees from X.
    pub infill_angle: f64,
}

impl Default for Print {
    fn default() -> Self {
        Print { layer_height: 0.2, outer_width: 0.42, inner_width: 0.45, wall_loops: 2, top_layers: 4, bottom_layers: 3, density: 0.15, infill: Infill::Gyroid, infill_angle: 45.0 }
    }
}

impl Print {
    /// The walls' thickness: the outer line, then the inner lines at their spacing.
    pub fn wall_thickness(&self) -> f64 {
        self.wall_thickness_of(self.wall_loops)
    }
    /// The thickness of `loops` walls of these lines.
    pub fn wall_thickness_of(&self, loops: usize) -> f64 {
        if loops == 0 {
            return 0.0;
        }
        let spacing = self.inner_width - self.layer_height * (1.0 - std::f64::consts::FRAC_PI_4);
        self.outer_width + (loops - 1) as f64 * spacing.max(0.0)
    }
    pub fn top_thickness(&self) -> f64 {
        self.top_layers as f64 * self.layer_height
    }
    pub fn bottom_thickness(&self) -> f64 {
        self.bottom_layers as f64 * self.layer_height
    }
}

/// Orthotropic constants in a material's own axes (MPa): moduli along 1, 2, 3, the shear
/// moduli of the 23, 13, 12 planes, and Poisson's ratios nu23, nu13, nu12.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ortho {
    pub e: [f64; 3],
    pub g: [f64; 3],
    pub nu: [f64; 3],
}

impl Ortho {
    pub fn isotropic(e: f64, nu: f64) -> Ortho {
        Ortho { e: [e; 3], g: [e / (2.0 * (1.0 + nu)); 3], nu: [nu; 3] }
    }

    /// The stiffness matrix: the compliance inverted.
    pub fn stiffness(&self) -> Mat6 {
        let [e1, e2, e3] = self.e;
        let [nu23, nu13, nu12] = self.nu;
        let mut s = [[0.0; 6]; 6];
        s[0][0] = 1.0 / e1;
        s[1][1] = 1.0 / e2;
        s[2][2] = 1.0 / e3;
        s[0][1] = -nu12 / e1;
        s[0][2] = -nu13 / e1;
        s[1][2] = -nu23 / e2;
        s[1][0] = s[0][1];
        s[2][0] = s[0][2];
        s[2][1] = s[1][2];
        s[3][3] = 1.0 / self.g[0];
        s[4][4] = 1.0 / self.g[1];
        s[5][5] = 1.0 / self.g[2];
        invert6(&s).expect("orthotropic compliance is positive definite")
    }
}

/// Strengths in a material's own axes (MPa): tensile along 1, 2, 3, shear in 23, 13, 12.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Strength {
    pub x: [f64; 3],
    pub s: [f64; 3],
}

impl Strength {
    /// Hill's index of a stress in these axes (1 at failure).
    pub fn hill(&self, s: &[f64; 6]) -> f64 {
        Hill::of(self).index(s)
    }
}

/// Hill's criterion of a [`Strength`]: its coefficients, F G H on the normal stresses' differences
/// and L M N on the shears.
#[derive(Clone, Copy, Debug)]
pub struct Hill([f64; 6]);

impl Hill {
    pub fn of(strength: &Strength) -> Hill {
        let [x, y, z] = strength.x.map(|v| 1.0 / (v * v));
        let [l, m, n] = strength.s.map(|v| 0.5 / (v * v));
        Hill([0.5 * (y + z - x), 0.5 * (z + x - y), 0.5 * (x + y - z), l, m, n])
    }

    /// The index of a stress in its strength's axes (1 at failure).
    pub fn index(&self, s: &[f64; 6]) -> f64 {
        let [f, g, h, l, m, n] = self.0;
        f * (s[1] - s[2]).powi(2) + g * (s[2] - s[0]).powi(2) + h * (s[0] - s[1]).powi(2)
            + 2.0 * (l * s[3] * s[3] + m * s[4] * s[4] + n * s[5] * s[5])
    }
}

/// Gauss-Jordan inverse of a symmetric positive definite 6x6 matrix.
pub fn invert6(m: &Mat6) -> Option<Mat6> {
    let mut a = *m;
    let mut inv = [[0.0; 6]; 6];
    for (i, row) in inv.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for c in 0..6 {
        let p = (c..6).max_by(|&i, &j| a[i][c].abs().total_cmp(&a[j][c].abs()))?;
        if a[p][c].abs() < 1e-300 {
            return None;
        }
        a.swap(c, p);
        inv.swap(c, p);
        let d = a[c][c];
        for k in 0..6 {
            a[c][k] /= d;
            inv[c][k] /= d;
        }
        for r in 0..6 {
            if r != c {
                let f = a[r][c];
                if f != 0.0 {
                    for k in 0..6 {
                        a[r][k] -= f * a[c][k];
                        inv[r][k] -= f * inv[c][k];
                    }
                }
            }
        }
    }
    Some(inv)
}

/// The Bond matrix of axes `a` (rows: the new axes in the old frame): stresses (Voigt)
/// in the new axes are `bond(a) * stress`; a stiffness given in the old axes is
/// `bond(a) D bond(a)^T` in the new ones.
pub fn bond(a: [[f64; 3]; 3]) -> Mat6 {
    let mut m = [[0.0; 6]; 6];
    let pairs = [(1, 2), (0, 2), (0, 1)];
    for i in 0..3 {
        for j in 0..3 {
            m[i][j] = a[i][j] * a[i][j];
        }
        for (k, &(p, q)) in pairs.iter().enumerate() {
            m[i][3 + k] = 2.0 * a[i][p] * a[i][q];
        }
    }
    for (r, &(p, q)) in pairs.iter().enumerate() {
        for j in 0..3 {
            m[3 + r][j] = a[p][j] * a[q][j];
        }
        for (k, &(s, t)) in pairs.iter().enumerate() {
            m[3 + r][3 + k] = a[p][s] * a[q][t] + a[p][t] * a[q][s];
        }
    }
    m
}

/// Axes turned `theta` (radians) about Z: a material's own axes in the printer frame.
pub fn turned(theta: f64) -> [[f64; 3]; 3] {
    let (s, c) = theta.sin_cos();
    [[c, s, 0.0], [-s, c, 0.0], [0.0, 0.0, 1.0]]
}

fn transpose3(a: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    std::array::from_fn(|i| std::array::from_fn(|j| a[j][i]))
}

/// `D` given in axes turned `theta` about Z, in the printer frame.
pub fn rotate_z(d: &Mat6, theta: f64) -> Mat6 {
    let m = bond(transpose3(turned(theta)));
    let md: Mat6 = std::array::from_fn(|r| std::array::from_fn(|c| (0..6).map(|k| m[r][k] * d[k][c]).sum()));
    std::array::from_fn(|r| std::array::from_fn(|c| (0..6).map(|k| md[r][k] * m[c][k]).sum()))
}

pub fn add6(a: &Mat6, b: &Mat6, k: f64) -> Mat6 {
    std::array::from_fn(|r| std::array::from_fn(|c| a[r][c] + k * b[r][c]))
}

pub fn scale6(a: &Mat6, k: f64) -> Mat6 {
    std::array::from_fn(|r| std::array::from_fn(|c| a[r][c] * k))
}

/// Angle bins for the walls' bead direction over half a turn.
pub const ANGLES: usize = 256;

/// The bin of a bead direction (radians, any): the direction is a line, period pi.
pub fn angle_bin(theta: f64) -> u8 {
    let t = theta.rem_euclid(std::f64::consts::PI) / std::f64::consts::PI;
    ((t * ANGLES as f64).round() as usize % ANGLES) as u8
}

pub fn bin_angle(bin: u8) -> f64 {
    bin as f64 * std::f64::consts::PI / ANGLES as f64
}

/// A volume of a part printed with settings of its own (a script's `m.settings`): inside it
/// the infill's density and pattern and the wall count it sets, the part's where it does not.
#[derive(Clone)]
pub struct Zone {
    /// At a printer point (mm): negative inside.
    pub at: Arc<dyn Fn(V3) -> f64 + Send + Sync>,
    /// The printer box it holds in (mm); `None`: it may hold anywhere.
    pub bounds: Option<(V3, V3)>,
    /// The infill's density (0..1).
    pub density: Option<f64>,
    pub infill: Option<Infill>,
    pub wall_loops: Option<usize>,
}

impl Zone {
    /// Whether it holds at printer point `p` (mm).
    pub fn holds(&self, p: V3) -> bool {
        self.bounds.is_none_or(|(lo, hi)| (0..3).all(|a| p[a] >= lo[a] && p[a] <= hi[a])) && (self.at)(p) < 0.0
    }
}

impl std::fmt::Debug for Zone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Zone").field("bounds", &self.bounds).field("density", &self.density).field("infill", &self.infill).field("wall_loops", &self.wall_loops).finish()
    }
}

/// The infill at one density and pattern: its stiffness in the printer frame and in the axes
/// of the infill direction, and its strength there.
#[derive(Clone, Debug)]
pub struct Fill {
    pub density: f64,
    pub d: Mat6,
    pub local: Mat6,
    pub strength: Strength,
}

/// The three materials of one part as it will print, ready for the solver: each material's
/// stiffness in the printer frame (the walls' for each bead direction bin) and what its
/// strength check needs.
#[derive(Clone, Debug)]
pub struct Materials {
    pub filament: Filament,
    pub print: Print,
    /// False: the part as solid isotropic polymer (the comparison the Force panel offers).
    pub as_printed: bool,
    pub wall: Vec<Mat6>,
    pub skin: Mat6,
    /// The bead in its own axes: its stiffness and strength (walls and skin plies).
    pub bead: Mat6,
    pub bead_strength: Strength,
    /// The infills, by a Gauss point's [`crate::grid::Gp::infill`]: the part's own first.
    /// With zones ([`Materials::with_zones`]), each density they or the part set with each
    /// pattern they or the part set: a zone may set the one and a later one over it the
    /// other. The table's layout follows the zones alone, so a grid graded under one print
    /// reads alike under another with the same zones ([`Materials::with_print`]).
    pub fills: Vec<Fill>,
    /// The infill (and skin) direction, radians.
    pub infill_angle: f64,
    /// The walls' and the skins' thicknesses (mm), for the grid's classification.
    pub wall_thickness: f64,
    pub top_thickness: f64,
    pub bottom_thickness: f64,
    /// The part's zones with settings of their own, in order.
    pub zones: Vec<Zone>,
    /// Per zone, the density and the pattern it sets, as their places in `fills`' table
    /// (density by pattern).
    sets: Vec<(Option<usize>, Option<usize>)>,
    /// The patterns in that table.
    patterns: usize,
}

impl Materials {
    pub fn new(filament: Filament, print: &Print, as_printed: bool) -> Materials {
        let f = filament;
        let theta = print.infill_angle.to_radians();
        if !as_printed {
            let e = 0.5 * (f.e_long + f.e_trans);
            let d = Ortho::isotropic(e, f.nu).stiffness();
            let x = f.x_long.min(f.x_trans);
            let strength = Strength { x: [x; 3], s: [x / 3f64.sqrt(); 3] };
            let fills = vec![Fill { density: print.density, d, local: d, strength }];
            return Materials {
                filament,
                print: print.clone(),
                as_printed,
                wall: vec![d; ANGLES],
                skin: d,
                bead: d,
                bead_strength: strength,
                fills,
                infill_angle: theta,
                wall_thickness: 0.0,
                top_thickness: 0.0,
                bottom_thickness: 0.0,
                zones: Vec::new(),
                sets: Vec::new(),
                patterns: 1,
            };
        }
        let w = print.inner_width.max(0.05);
        let h = print.layer_height.clamp(0.02, w);
        let v = (V_REF * (h / w) / (H_REF / W_REF)).clamp(0.0, 0.3);
        let (e1, e2) = ((1.0 - v) * f.e_long, (1.0 - v.sqrt()) * f.e_trans);
        let e3 = f.k_layer * e2;
        let g = e2 / (2.0 * (1.0 + f.nu));
        let ply = Ortho { e: [e1, e2, e3], g: [f.k_layer * g, g, g], nu: [f.nu; 3] };
        let bead = ply.stiffness();
        let x_layer = f.x_layer_of(h, w);
        let bead_strength = Strength {
            x: [f.x_long, f.x_trans, x_layer],
            s: [x_layer / 3f64.sqrt(), x_layer / 3f64.sqrt(), f.x_trans / 3f64.sqrt()],
        };
        let wall = (0..ANGLES).map(|b| rotate_z(&bead, bin_angle(b as u8))).collect();
        let mut m = Materials {
            filament,
            print: print.clone(),
            as_printed,
            wall,
            skin: rotate_z(&skin_local(&bead), theta),
            bead,
            bead_strength,
            fills: Vec::new(),
            infill_angle: theta,
            wall_thickness: print.wall_thickness(),
            top_thickness: print.top_thickness(),
            bottom_thickness: print.bottom_thickness(),
            zones: Vec::new(),
            sets: Vec::new(),
            patterns: 1,
        };
        m.fills = vec![m.fill(print.density, print.infill)];
        m
    }

    /// The infill at `density` in `pattern`, of these plies: the pattern's law scales the
    /// skin's stiffness and the beads' strengths.
    fn fill(&self, density: f64, pattern: Infill) -> Fill {
        if !self.as_printed {
            return Fill { density, d: self.skin, local: self.skin, strength: self.bead_strength };
        }
        let f = &self.filament;
        let x_layer = self.bead_strength.x[2];
        let skin_local = skin_local(&self.bead);
        let rho = density.clamp(0.0, 1.0);
        let laws = pattern.laws(f);
        let k: [f64; 6] = laws.map(|(n, c)| rho.powf(n) * (c + (1.0 - c) * rho));
        // a floor that keeps empty infill (0 %) a solvable, negligible material
        let l: [f64; 6] = k.map(|k| k.max(1e-4).sqrt());
        let local: Mat6 = std::array::from_fn(|r| std::array::from_fn(|c| l[r] * skin_local[r][c] * l[c]));
        let ks: [f64; 6] = laws.map(|(n, c)| rho.powf(0.5 + 0.5 * n) * (c + (1.0 - c) * rho));
        let strength = Strength {
            x: [ks[0] * f.x_long, ks[1] * f.x_long, ks[2] * x_layer].map(|x| x.max(1e-3)),
            s: [ks[3] * x_layer / 3f64.sqrt(), ks[4] * x_layer / 3f64.sqrt(), ks[5] * f.x_trans / 3f64.sqrt()].map(|x| x.max(1e-3)),
        };
        Fill { density, d: rotate_z(&local, self.infill_angle), local, strength }
    }

    /// These materials with the part's zones of settings of their own, in order: where two
    /// overlap, the later one's value of a key wins.
    pub fn with_zones(mut self, zones: Vec<Zone>) -> Materials {
        // the part's own first, then the zones' values, each once
        let (mut densities, mut patterns) = (vec![self.print.density], vec![self.print.infill]);
        self.sets = zones.iter().map(|r| (r.density.map(|d| place(&mut densities, d)), r.infill.map(|p| place(&mut patterns, p)))).collect();
        let fills: Vec<Fill> = densities.iter().flat_map(|&d| patterns.iter().map(move |&p| (d, p))).map(|(d, p)| self.fill(d, p)).collect();
        assert!(fills.len() <= 1 << 16, "{} densities by {} patterns: more infills than a Gauss point names", densities.len(), patterns.len());
        self.fills = fills;
        self.patterns = patterns.len();
        self.zones = zones;
        self
    }

    /// These materials under other print settings: the same filament and zones, as printed or
    /// not as these are (an optimiser's candidate, a what-if of the infill).
    pub fn with_print(&self, print: &Print) -> Materials {
        Materials::new(self.filament, print, self.as_printed).with_zones(self.zones.clone())
    }

    /// What the part prints with at printer point `p` (mm): its infill (its place in
    /// [`Materials::fills`]) and its walls' thickness, the zones' where they hold.
    pub fn at(&self, p: V3) -> (u16, f64) {
        let (mut density, mut pattern, mut walls) = (0, 0, None);
        for (r, (d, q)) in self.zones.iter().zip(&self.sets) {
            if r.holds(p) {
                density = d.unwrap_or(density);
                pattern = q.unwrap_or(pattern);
                walls = r.wall_loops.or(walls);
            }
        }
        let thickness = match walls {
            Some(n) if self.as_printed => self.print.wall_thickness_of(n),
            _ => self.wall_thickness,
        };
        ((density * self.patterns + pattern) as u16, thickness)
    }

    /// The stiffness of a Gauss point's mixture: `wall` and `skin` its shares (0..1), the
    /// rest infill `fill` (its place in [`Materials::fills`]), the walls' beads in direction
    /// bin `angle`.
    pub fn mix(&self, wall: f64, skin: f64, angle: u8, fill: u16) -> Mat6 {
        let infill = (1.0 - wall - skin).max(0.0);
        let mut d = scale6(&self.fills[fill as usize].d, infill);
        if wall > 0.0 {
            d = add6(&d, &self.wall[angle as usize], wall);
        }
        if skin > 0.0 {
            d = add6(&d, &self.skin, skin);
        }
        d
    }

    /// The strength checks of a mixture: each material in it (the walls along bead direction
    /// bin `angle`, the skin's two plies, the infill `fill`), where it fills at least a
    /// twentieth.
    pub fn checks(&self, wall: f64, skin: f64, angle: u8, fill: u16) -> Checks {
        if !self.as_printed {
            return Checks(vec![(self.skin, Hill::of(&self.bead_strength))]);
        }
        const PRESENT: f64 = 0.05;
        let mut each = Vec::new();
        if wall >= PRESENT {
            // the walls' stress, turned into the bead's axes
            each.push((mul66(&bond(turned(bin_angle(angle))), &self.wall[angle as usize]), Hill::of(&self.bead_strength)));
        }
        if skin >= PRESENT {
            for t in [self.infill_angle, self.infill_angle + std::f64::consts::FRAC_PI_2] {
                each.push((mul66(&self.bead, &strain_to_axes(t)), Hill::of(&self.bead_strength)));
            }
        }
        if 1.0 - wall - skin >= PRESENT {
            let fill = &self.fills[fill as usize];
            each.push((mul66(&fill.local, &strain_to_axes(self.infill_angle)), Hill::of(&fill.strength)));
        }
        Checks(each)
    }
}

/// The skin's plies at the infill direction and across it, in the infill's axes: the average
/// of the bead and the bead turned a quarter.
fn skin_local(bead: &Mat6) -> Mat6 {
    scale6(&add6(bead, &rotate_z(bead, std::f64::consts::FRAC_PI_2), 1.0), 0.5)
}

/// The place of `v` in `list` past its first entry (the part's own, which a zone's value never
/// shares, however equal), added at its end when it is not there.
fn place<T: PartialEq>(list: &mut Vec<T>, v: T) -> usize {
    match list.iter().skip(1).position(|x| *x == v) {
        Some(i) => i + 1,
        None => {
            list.push(v);
            list.len() - 1
        }
    }
}

/// A mixture's strength checks ([`Materials::checks`]): per material, the stress in its own axes
/// of a printer-frame strain, and its criterion there.
#[derive(Clone, Debug)]
pub struct Checks(Vec<(Mat6, Hill)>);

impl Checks {
    /// Hill's index of the weakest material under printer-frame strain `e`.
    pub fn failure(&self, e: &[f64; 6]) -> f64 {
        self.0.iter().map(|(m, hill)| hill.index(&mul6(m, e))).fold(0.0, f64::max)
    }
}

fn mul66(a: &Mat6, b: &Mat6) -> Mat6 {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..6).map(|k| a[r][k] * b[k][c]).sum()))
}

/// A printer-frame strain (Voigt, engineering shears) in axes turned `theta` about Z, as a
/// matrix: engineering strain transforms with bond(a)^-T, so the shears are halved into tensor
/// form, turned as a stress and doubled back.
fn strain_to_axes(theta: f64) -> Mat6 {
    let b = bond(turned(theta));
    std::array::from_fn(|r| std::array::from_fn(|c| b[r][c] * if c >= 3 { 0.5 } else { 1.0 } * if r >= 3 { 2.0 } else { 1.0 }))
}

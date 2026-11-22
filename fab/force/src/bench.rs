//! Parts with known answers or known difficulty, built from distance fields the way the
//! Force render mode builds a part, for the tests and for `fab-forcebench` (the solver's iteration
//! counts and times on them).
//!
//! - [`cantilever`]: a 24 mm beam of 4 x 4 mm section, clamped at one end, 1 N down on the
//!   other (beam theory gives its tip deflection and bending stress);
//! - [`kirsch`]: a quarter of a strip with a hole, pulled (Heywood's stress concentration);
//! - [`bracket`]: the demo plate's shelf bracket at its script's defaults (an L of 6 mm
//!   plates, a filleted inside corner, four screw holes), held at the wall's back and loaded
//!   on the shelf: thin plates, the solver's hard case.

use crate::bc::{Condition, Kind};
use crate::grid::{Grid, Lattice};
use crate::material::{Filament, Materials, Print};
use crate::study::{Load, Spot};
use crate::surface::{Pick, Region, Surface};
use crate::V3;

pub const E: f64 = 1000.0;
pub const NU: f64 = 0.3;

/// An isotropic material, E 1000 MPa, nu 0.3, strength 50 MPa.
pub fn isotropic() -> Materials {
    let f = Filament { name: "test", e_long: E, e_trans: E, nu: NU, k_layer: 1.0, x_long: 50.0, x_trans: 50.0, x_layer: 50.0 };
    Materials::new(f, &Print::default(), false)
}

/// The exact distance to the box `lo..hi` (negative inside).
pub fn boxed(p: V3, lo: V3, hi: V3) -> f64 {
    let q: V3 = std::array::from_fn(|a| (lo[a] - p[a]).max(p[a] - hi[a]));
    let out = (q.iter().map(|v| v.max(0.0).powi(2)).sum::<f64>()).sqrt();
    out + q[0].max(q[1]).max(q[2]).min(0.0)
}

/// A part from its field over `lo..hi` at cells of `h`: the grid, the lattice, the surface.
pub fn part(lo: V3, hi: V3, h: f64, m: &Materials, field: impl Fn(V3) -> f64) -> (Grid, Lattice, Surface) {
    let mut distance = |ps: &[V3], out: &mut [f64]| {
        for (p, o) in ps.iter().zip(out.iter_mut()) {
            *o = field(*p);
        }
    };
    let (grid, lattice) = Grid::from_field(lo, hi, h, &mut distance, m);
    let surface = Surface::contour(&lattice, &mut crate::surface::field_normals(&mut distance, 1e-3 * lattice.spacing));
    (grid, lattice, surface)
}

/// The region a click at `p` (on the surface) picks.
pub fn pick(s: &Surface, p: V3, how: Pick) -> Region {
    s.grow(s.nearest(p).expect("a surface"), how)
}

/// Rollers: hold axis `a` on every node on the plane `x_a = at`.
pub fn roller(g: &Grid, held: &mut [u8], a: usize, at: f64) {
    for n in 0..g.nodes() {
        if (g.node_pos(n)[a] - at).abs() < 1e-9 {
            held[n] |= 1 << a;
        }
    }
}

/// A tall gyroid shell over its held foot at cells of `h`: its walls are a cell or two across,
/// and 5 N push its top sideways. Its grid, held nodes and forces.
pub fn lattice(h: f64, m: &Materials) -> (Grid, Vec<u8>, Vec<V3>) {
    let (lo, hi) = ([0.0; 3], [16.0 * h, 16.0 * h, 32.0 * h]);
    let k = std::f64::consts::TAU / (8.0 * h);
    let (grid, _, _) = part(lo, hi, h, m, |p| {
        let [x, y, z] = p.map(|v| k * v);
        let shell = (x.sin() * y.cos() + y.sin() * z.cos() + z.sin() * x.cos()).abs() / k;
        (shell - 0.6 * h).max(boxed(p, lo, hi))
    });
    let touched = |g: &Grid| {
        let mut on = vec![false; g.nodes()];
        for c in (0..g.cells()).filter(|&c| g.active(c)) {
            g.cell_nodes(c).iter().for_each(|&n| on[n] = true);
        }
        on
    };
    let on = touched(&grid);
    let at = |n: usize, z: f64| on[n] && (grid.node_pos(n)[2] - z).abs() < 0.5 * h;
    let held = (0..grid.nodes()).map(|n| if at(n, lo[2]) { 7 } else { 0 }).collect();
    let top: Vec<usize> = (0..grid.nodes()).filter(|&n| at(n, hi[2])).collect();
    let mut force = vec![[0.0; 3]; grid.nodes()];
    top.iter().for_each(|&n| force[n] = [5.0 / top.len() as f64, 0.0, 0.0]);
    (grid, held, force)
}

/// The cantilever's length and section side (mm).
pub const BEAM: (f64, f64) = (24.0, 4.0);

/// The cantilever at cells of `h`: its grid, surface and conditions (root clamped, 1 N down
/// on the tip face).
pub fn cantilever(h: f64, m: &Materials) -> (Grid, Surface, Vec<Condition>) {
    cantilever_at(h, [0.0; 3], m)
}

/// [`cantilever`] on its grid moved by `shift` ([`crate::study::shift`]).
pub fn cantilever_at(h: f64, shift: V3, m: &Materials) -> (Grid, Surface, Vec<Condition>) {
    let (l, s) = BEAM;
    let (lo, hi) = ([0.0; 3], [l, s, s]);
    let (grid, _, surface) = part(std::array::from_fn(|a| lo[a] + shift[a]), std::array::from_fn(|a| hi[a] + shift[a]), h, m, |p| boxed(p, lo, hi));
    let root = pick(&surface, [0.0, 0.5 * s, 0.5 * s], Pick::Face);
    let tip = pick(&surface, [l, 0.5 * s, 0.5 * s], Pick::Face);
    let conditions = vec![Condition { kind: Kind::Fixed, region: root }, Condition { kind: Kind::Force([0.0, 0.0, -1.0]), region: tip }];
    (grid, surface, conditions)
}

/// The cantilever's tip deflection by beam theory (Timoshenko, shear included).
pub fn cantilever_tip() -> f64 {
    let (l, s) = BEAM;
    let i = s * s.powi(3) / 12.0;
    let g = E / (2.0 * (1.0 + NU));
    l.powi(3) / (3.0 * E * i) + l / (5.0 / 6.0 * g * s * s)
}

/// Kirsch's plate, a quarter of it (symmetry rollers on its cut planes): `l` x `w` x `t` mm
/// with a hole of radius `a` at the corner, pulled at `pull` MPa along x on its far end.
/// Its grid, holds and forces.
pub fn kirsch(a: f64, l: f64, w: f64, t: f64, h: f64, pull: f64, m: &Materials) -> (Grid, Vec<u8>, Vec<V3>) {
    let (lo, hi) = ([0.0; 3], [l, w, t]);
    let (grid, _, surface) = part(lo, hi, h, m, |p| boxed(p, lo, hi).max(a - (p[0] * p[0] + p[1] * p[1]).sqrt()));
    let end = pick(&surface, [l, 0.5 * w, 0.5 * t], Pick::Face);
    let (_, force) = crate::bc::nodes(&grid, &surface, &[Condition { kind: Kind::Force([pull * w * t, 0.0, 0.0]), region: end }]);
    let mut held = vec![0u8; grid.nodes()];
    for axis in 0..3 {
        roller(&grid, &mut held, axis, 0.0);
    }
    (grid, held, force)
}

/// The bracket's box (printer mm): 64 wide (x), 46 deep (y), 52 high (z).
pub const BRACKET: (V3, V3) = ([-32.0, -23.0, 0.0], [32.0, 23.0, 52.0]);

/// The demo shelf bracket at its script's defaults, as printed (the shelf on the plate, the
/// wall at the back, y = -23): 6 mm plates, a 9 mm inside fillet, two 4.6 mm holes down
/// through the shelf and two through the wall (the counterbores and edge rounding left out).
pub fn bracket(p: V3) -> f64 {
    bracket_filleted(p, 9.0)
}

/// [`bracket`] with an inside fillet of radius `r` (a shape edit that leaves its study's faces
/// where they were).
pub fn bracket_filleted(p: V3, r: f64) -> f64 {
    let (y, z) = (p[1], p[2]);
    // the section (y, z), its exact distance (FORCE A13): the L's hull less the air's corner
    // (u = y + 17 > 0, v = z - 6 > 0) opened by `r`, so no two pieces touch along a shared side
    let hull = rect(y, z, [-23.0, 0.0], [23.0, 52.0]);
    let section = hull.max(r - quadrant(y + 17.0 - r, z - 6.0 - r));
    let q = [section, p[0].abs() - 32.0];
    let mut d = (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt() + q[0].max(q[1]).min(0.0);
    let (r, x) = (2.3, 0.27 * 64.0);
    for sx in [-x, x] {
        d = d.max(r - ((p[0] - sx).powi(2) + (y - 7.5).powi(2)).sqrt());
        d = d.max(r - ((p[0] - sx).powi(2) + (z - 35.35).powi(2)).sqrt());
    }
    d
}

/// A rectangle's exact distance in a plane, inside as well as out (a box a millimetre wide along x
/// would read -1 mm everywhere deeper: a flat field, every cell of the core a skin's depth).
pub fn rect(y: f64, z: f64, lo: [f64; 2], hi: [f64; 2]) -> f64 {
    let q = [(lo[0] - y).max(y - hi[0]), (lo[1] - z).max(z - hi[1])];
    (q[0].max(0.0).powi(2) + q[1].max(0.0).powi(2)).sqrt() + q[0].max(q[1]).min(0.0)
}

/// The exact distance of the quadrant `a > 0, b > 0`, negative inside.
pub fn quadrant(a: f64, b: f64) -> f64 {
    if a > 0.0 && b > 0.0 { -a.min(b) } else { a.min(0.0).hypot(b.min(0.0)) }
}

/// The bracket's study: its wall's back face held (screwed to a wall), 100 N down on the
/// shelf's top.
pub fn bracket_loads() -> [Load; 2] {
    [
        Load::new(Kind::Fixed, Spot::Seed { pick: Pick::Face, at: [0.0, -23.0, 26.0] }),
        Load::new(Kind::Force([0.0, 0.0, -100.0]), Spot::Seed { pick: Pick::Face, at: [0.0, 10.0, 6.0] }),
    ]
}

/// A rotation (a matrix taking a member's own frame to the printer's): turned by `turn`
/// (radians) about z, then as much about y, so no face of the member is square to the grid.
pub fn turned(turn: f64) -> [[f64; 3]; 3] {
    let (s, c) = turn.sin_cos();
    let rz = [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]];
    let ry = [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]];
    std::array::from_fn(|r| std::array::from_fn(|k| (0..3).map(|j| ry[r][j] * rz[j][k]).sum()))
}

fn rotate(r: &[[f64; 3]; 3], v: V3) -> V3 {
    std::array::from_fn(|a| r[a][0] * v[0] + r[a][1] * v[1] + r[a][2] * v[2])
}

fn unrotate(r: &[[f64; 3]; 3], v: V3) -> V3 {
    std::array::from_fn(|a| r[0][a] * v[0] + r[1][a] * v[1] + r[2][a] * v[2])
}

/// A straight member for the buckling bench: in its own frame along x from 0 to `l`, its
/// section `size` (y by z; round: a circle of diameter `size[0]`) about the axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Member {
    pub l: f64,
    pub size: [f64; 2],
    pub round: bool,
    /// Its frame in the printer's ([`turned`]).
    pub frame: [[f64; 3]; 3],
}

impl Member {
    /// The member's exact distance (negative inside) at a printer-frame point.
    pub fn distance(&self, p: V3) -> f64 {
        let q = unrotate(&self.frame, p);
        let (l, [a, b]) = (self.l, self.size);
        if self.round {
            let dr = (q[1] * q[1] + q[2] * q[2]).sqrt() - 0.5 * a;
            let dx = (-q[0]).max(q[0] - l);
            (dr.max(0.0).powi(2) + dx.max(0.0).powi(2)).sqrt() + dr.max(dx).min(0.0)
        } else {
            boxed(q, [0.0, -0.5 * a, -0.5 * b], [l, 0.5 * a, 0.5 * b])
        }
    }

    /// A point of the member's own frame in the printer's.
    pub fn at(&self, local: V3) -> V3 {
        rotate(&self.frame, local)
    }

    /// The member's box in the printer's frame.
    pub fn bounds(&self) -> (V3, V3) {
        let [a, b] = self.size;
        let (mut lo, mut hi) = ([f64::MAX; 3], [f64::MIN; 3]);
        for k in 0..8 {
            let c = crate::element::corner(k);
            let p = self.at([c[0] as f64 * self.l, (c[1] as f64 - 0.5) * a, (c[2] as f64 - 0.5) * b]);
            for ax in 0..3 {
                lo[ax] = lo[ax].min(p[ax]);
                hi[ax] = hi[ax].max(p[ax]);
            }
        }
        (lo, hi)
    }

    /// The member at cells of `h` on its grid moved by `shift` ([`crate::study::shift`]),
    /// its root face fixed and `load` (N, its own frame) on its tip face, both picked on the
    /// surface as a click picks them: its grid, surface and conditions.
    pub fn posed(&self, load: V3, h: f64, shift: V3, m: &Materials) -> (Grid, Surface, Vec<Condition>) {
        self.posed_with(Kind::Force(rotate(&self.frame, load)), h, shift, m)
    }

    /// [`Member::posed`] with any load on the tip face (`tip`, in the printer's frame).
    pub fn posed_with(&self, tip: Kind, h: f64, shift: V3, m: &Materials) -> (Grid, Surface, Vec<Condition>) {
        let [a, b] = self.size;
        cantilever_of(&|q| self.distance(rotate(&self.frame, q)), [0.0, -0.5 * a, -0.5 * b], [self.l, 0.5 * a, 0.5 * b], [0.0; 3], [self.l, 0.0, 0.0], tip, self.frame, h, shift, m)
    }

    /// The second moments of area of the section about its y and z axes (mm^4), and its area.
    pub fn section(&self) -> (f64, f64, f64) {
        let [a, b] = self.size;
        if self.round {
            let i = std::f64::consts::PI * a.powi(4) / 64.0;
            (i, i, std::f64::consts::PI * a * a / 4.0)
        } else {
            (a * b.powi(3) / 12.0, b * a.powi(3) / 12.0, a * b)
        }
    }

    /// The Euler load (N) of the member as a column clamped at its root and free at its tip,
    /// pushed along its axis: `pi^2 E I / (4 L^2)` on its weaker axis, and with shear
    /// (Engesser: `P / (1 + P / (kappa G A))`, Cowper's shear coefficients `kappa`: Timoshenko
    /// and Gere, "Theory of Elastic Stability", 2nd ed. 1961, sections 2.1 and 2.17; Cowper,
    /// "The shear coefficient in Timoshenko's beam theory", J. Appl. Mech. 33, 1966; the
    /// coefficients checked against Wikipedia's "Timoshenko-Ehrenfest beam theory", its table
    /// of Cowper's coefficients).
    pub fn euler(&self, e: f64, nu: f64) -> f64 {
        let (iy, iz, area) = self.section();
        let p = std::f64::consts::PI.powi(2) * e * iy.min(iz) / (4.0 * self.l * self.l);
        let kappa = if self.round { 6.0 * (1.0 + nu) / (7.0 + 6.0 * nu) } else { 10.0 * (1.0 + nu) / (12.0 + 11.0 * nu) };
        let g = e / (2.0 * (1.0 + nu));
        p / (1.0 + p / (kappa * g * area))
    }
}

/// An isotropic material of modulus `e` (MPa, or any unit with the lengths') and Poisson's
/// ratio `nu`, strength 50.
pub fn isotropic_of(e: f64, nu: f64) -> Materials {
    let f = Filament { name: "test", e_long: e, e_trans: e, nu, k_layer: 1.0, x_long: 50.0, x_trans: 50.0, x_layer: 50.0 };
    Materials::new(f, &Print::default(), false)
}

/// Saint-Venant's torsion constant of an `a` by `b` rectangle (either way round): `(1/3) [1 -
/// (192 / pi^5) (a/b) sum over odd n of tanh(n pi b / (2a)) / n^5] a^3 b`, `a` the shorter
/// side (Timoshenko and Goodier, "Theory of Elasticity", 2nd ed. 1951, pp. 275-277; a square's
/// is 0.140577 a^4).
pub fn torsion_constant(a: f64, b: f64) -> f64 {
    let (a, b) = (a.min(b), a.max(b));
    let pi = std::f64::consts::PI;
    let sum: f64 = (0..50).map(|k| (2 * k + 1) as f64).map(|n| (n * pi * b / (2.0 * a)).tanh() / n.powi(5)).sum();
    (1.0 - 192.0 / pi.powi(5) * (a / b) * sum) * a.powi(3) * b / 3.0
}

/// A narrow strip for the lateral-torsional case: `l` long, `depth` deep along its own z (the
/// load's direction), `thick` across its own y.
pub fn strip(l: f64, depth: f64, thick: f64, frame: [[f64; 3]; 3]) -> Member {
    Member { l, size: [thick, depth], round: false, frame }
}

/// The lateral-torsional buckling load (N) of a narrow rectangular cantilever with an end load
/// through the centroid of its tip section: `4.013 sqrt(E I_y G J) / L^2`, `I_y` the weak axis'
/// second moment, `J` Saint-Venant's torsion constant ([`torsion_constant`]); warping is nil
/// for a rectangle (Timoshenko and Gere, "Theory of Elastic Stability", 2nd ed. 1961, section
/// 6.3; the coefficient checked against a published paper's statement of it for an end load
/// through the centroid: NSF Public Access Repository, par.nsf.gov/servlets/purl/10410704).
pub fn lateral_torsional(member: &Member, e: f64, nu: f64) -> f64 {
    let [t, d] = member.size;
    let weak = d * t.powi(3) / 12.0;
    let g = e / (2.0 * (1.0 + nu));
    4.013 * (e * weak * g * torsion_constant(t, d)).sqrt() / (member.l * member.l)
}

/// A square plate `a` wide and `t` thick (x and y across, z through), simply supported on all
/// four edges and pressed along x by `sigma` MPa on its two x faces: its grid at cells of `h`
/// (`t / h` even, so its mid-plane is a node plane), held nodes and forces. The edges' mid-plane
/// nodes are held across the plate (z): their rotation is free, a simple support. In the plane
/// only the rigid motion is held: x along the line x = a/2 and y along y = a/2 of the
/// mid-plane, where the compressed plate's own movement is nil by symmetry and the mode's is
/// too.
pub fn plate(a: f64, t: f64, h: f64, sigma: f64, m: &Materials) -> (Grid, Vec<u8>, Vec<V3>) {
    let (lo, hi) = ([0.0; 3], [a, a, t]);
    let (grid, _, surface) = part(lo, hi, h, m, |p| boxed(p, lo, hi));
    let held = plate_holds(&grid, a, t, h);
    let f = sigma * a * t;
    let conditions = [
        Condition { kind: Kind::Force([f, 0.0, 0.0]), region: pick(&surface, [0.0, 0.5 * a, 0.5 * t], Pick::Face) },
        Condition { kind: Kind::Force([-f, 0.0, 0.0]), region: pick(&surface, [a, 0.5 * a, 0.5 * t], Pick::Face) },
    ];
    let (_, force) = crate::bc::nodes(&grid, &surface, &conditions);
    (grid, held, force)
}

/// [`plate`]'s holds: its mid-plane's edge lines held across it (a soft simple support: the
/// edges turn and twist freely), its middle lines held in x and in y against moving whole.
pub fn plate_holds(grid: &Grid, a: f64, t: f64, h: f64) -> Vec<u8> {
    let near = |v: f64, at: f64| (v - at).abs() < 1e-6 * h;
    let mut held = vec![0u8; grid.nodes()];
    for (n, held) in held.iter_mut().enumerate() {
        let p = grid.node_pos(n);
        if !near(p[2], 0.5 * t) {
            continue;
        }
        if near(p[0], 0.0) || near(p[0], a) || near(p[1], 0.0) || near(p[1], a) {
            *held |= 4;
        }
        if near(p[0], 0.5 * a) {
            *held |= 1;
        }
        if near(p[1], 0.5 * a) {
            *held |= 2;
        }
    }
    held
}

/// [`plate`] held the same way, pressed by `q` MPa on its top face instead.
pub fn plate_pressed(a: f64, t: f64, h: f64, q: f64, m: &Materials) -> (Grid, Vec<u8>, Vec<V3>) {
    let (lo, hi) = ([0.0; 3], [a, a, t]);
    let (grid, _, surface) = part(lo, hi, h, m, |p| boxed(p, lo, hi));
    let held = plate_holds(&grid, a, t, h);
    let top = surface.select(|_, n| n[2] > 0.9);
    let (_, force) = crate::bc::nodes(&grid, &surface, &[Condition { kind: Kind::Pressure(q), region: top }]);
    (grid, held, force)
}

/// The centre deflection (mm) of a simply supported square plate `a` wide and `t` thick under
/// `q` MPa, by Kirchhoff's thin-plate theory: `0.00406 q a^4 / D`, `D = E t^3 / (12 (1 -
/// nu^2))` (Navier's series; the coefficient read on LIRA-FEM's verification test 1.46,
/// help.liraland.com, which cites Birger and Panovko, "Strength, stability, vibrations", vol. 1,
/// 1968, pp. 532-535).
pub fn plate_deflection(a: f64, t: f64, q: f64, e: f64, nu: f64) -> f64 {
    0.00406 * q * a.powi(4) / (e * t.powi(3) / (12.0 * (1.0 - nu * nu)))
}

/// [`plate_critical`] with the plate's shear (Mindlin's plate, one half-wave each way, shear
/// factor 5/6): `k / (1 + pi^2 (t/a)^2 / (3 (1 - nu) 5/6))`, from `N = D pi^2 (2/a^2)^2 a^2 / (1 +
/// D pi^2 (2/a^2) / (kappa G t))` with `D / (G t) = t^2 / (6 (1 - nu))`. A rough value beside
/// the reference, not one.
pub fn plate_critical_mindlin(a: f64, t: f64, e: f64, nu: f64) -> f64 {
    let pi2 = std::f64::consts::PI.powi(2);
    plate_critical(a, t, e, nu) / (1.0 + pi2 * (t / a).powi(2) / (3.0 * (1.0 - nu) * 5.0 / 6.0))
}

/// The critical stress (MPa) of a simply supported square plate pressed along one axis:
/// `k pi^2 E / (12 (1 - nu^2)) (t/b)^2` with `k = 4` (Timoshenko and Gere, "Theory of Elastic
/// Stability", 2nd ed. 1961, section 9.2; checked against structville.com, "Buckling of thin
/// plates", 2022: k = 4.0 for one half-wave, a square plate).
pub fn plate_critical(a: f64, t: f64, e: f64, nu: f64) -> f64 {
    4.0 * std::f64::consts::PI.powi(2) * e / (12.0 * (1.0 - nu * nu)) * (t / a).powi(2)
}

/// A part held at one face and loaded on another, for the verification rig: `field` its
/// distance in its own frame, `lo..hi` its box there, `root` and `tip` points on the faces held
/// and loaded (`tip` the load, printer frame), placed in the printer's frame by `frame`
/// ([`turned`]), at cells of `h` on a grid moved by `shift`. Both faces picked as a click picks
/// them.
#[allow(clippy::too_many_arguments)]
pub fn cantilever_of(field: &dyn Fn(V3) -> f64, lo: V3, hi: V3, root: V3, tip: V3, load: Kind, frame: [[f64; 3]; 3], h: f64, shift: V3, m: &Materials) -> (Grid, Surface, Vec<Condition>) {
    let (blo, bhi) = turned_bounds(lo, hi, &frame);
    let (grid, _, surface) = part(std::array::from_fn(|a| blo[a] + shift[a]), std::array::from_fn(|a| bhi[a] + shift[a]), h, m, |p| field(unrotate(&frame, p)));
    let root = pick(&surface, rotate(&frame, root), Pick::Face);
    let tip = pick(&surface, rotate(&frame, tip), Pick::Face);
    (grid, surface, vec![Condition { kind: Kind::Fixed, region: root }, Condition { kind: load, region: tip }])
}

/// The printer-frame box of the box `lo..hi` of a part's own frame placed by `frame`.
pub fn turned_bounds(lo: V3, hi: V3, frame: &[[f64; 3]; 3]) -> (V3, V3) {
    let (mut blo, mut bhi) = ([f64::MAX; 3], [f64::MIN; 3]);
    for k in 0..8 {
        let c = crate::element::corner(k);
        let p = rotate(frame, std::array::from_fn(|a| if c[a] == 0 { lo[a] } else { hi[a] }));
        for a in 0..3 {
            blo[a] = blo[a].min(p[a]);
            bhi[a] = bhi[a].max(p[a]);
        }
    }
    (blo, bhi)
}

/// A vector of a part's own frame in the printer's, and back.
pub fn to_printer(frame: &[[f64; 3]; 3], v: V3) -> V3 {
    rotate(frame, v)
}
pub fn to_own(frame: &[[f64; 3]; 3], v: V3) -> V3 {
    unrotate(frame, v)
}

/// MacNeal and Harder's curved beam (in its own frame): a quarter ring of radii 4.12 to 4.32
/// about the z axis, 0.1 wide along z, from its root face on y = 0 to its tip face on x = 0
/// (MacNeal and Harder, "A proposed standard set of problems to test finite element accuracy",
/// Finite Elements in Analysis and Design 1, 1985).
pub const CURVED: (f64, f64, f64) = (4.12, 4.32, 0.1);

pub fn curved_beam(p: V3) -> f64 {
    let (ri, ro, w) = CURVED;
    let r = (p[0] * p[0] + p[1] * p[1]).sqrt();
    (r - ro).max(ri - r).max(p[2].abs() - 0.5 * w).max(-p[0]).max(-p[1])
}

/// MacNeal and Harder's twisted beam (in its own frame): 12 long along x, its 1.1 by 0.32
/// section (width along y and thickness along z at the root) turning a quarter turn about x
/// from root to tip.
pub const TWISTED: (f64, f64, f64) = (12.0, 1.1, 0.32);

pub fn twisted_beam(p: V3) -> f64 {
    let (l, w, t) = TWISTED;
    let (s, c) = (std::f64::consts::FRAC_PI_2 * (p[0] / l).clamp(0.0, 1.0)).sin_cos();
    let (qy, qz) = (p[1] * c + p[2] * s, -p[1] * s + p[2] * c);
    (qy.abs() - 0.5 * w).max(qz.abs() - 0.5 * t).max(-p[0]).max(p[0] - l)
}

/// An ellipse's signed distance, to first order (exact on it, close near it): `(x/a)^2 +
/// (y/b)^2 - 1` over its gradient's length.
pub fn ellipse(x: f64, y: f64, a: f64, b: f64) -> f64 {
    let g = (x / a).powi(2) + (y / b).powi(2) - 1.0;
    let n = ((2.0 * x / (a * a)).powi(2) + (2.0 * y / (b * b)).powi(2)).sqrt();
    g / n.max(1e-12)
}

/// The NAFEMS LE1 and LE10 part (mm): a quarter of an elliptic plate `t` thick, its outer edge
/// the ellipse of semi-axes 3250 and 2750, its hole's 2000 and 1000, x and y from 0 (The
/// Standard NAFEMS Benchmarks, TNSB Rev. 3, 1990).
pub fn nafems_plate(p: V3, t: f64) -> f64 {
    ellipse(p[0], p[1], 3250.0, 2750.0).max(-ellipse(p[0], p[1], 2000.0, 1000.0)).max((p[2] - 0.5 * t).abs() - 0.5 * t).max(-p[0]).max(-p[1])
}

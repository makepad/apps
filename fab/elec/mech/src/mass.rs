use crate::BoardMech;
use fab_elec_core::geom::{orient, NM_PER_MM};
use fab_elec_core::{to_mm, Contour, PolySet, Side, P};

/// Density of the glass-fibre board laminate in g/mm³ (1.85 g/cm³).
const LAMINATE_DENSITY: f64 = 0.00185;
/// A part's body over its whole box, in g/mm³: moulded package, metal and some air, which weigh
/// about 1.2 to 2.1 g per cm³ of the box; 1.6 is the middle.
const BODY_DENSITY: f64 = 0.0016;

/// A board's mass properties in the [`BoardMech`] frame, in millimetres and grams.
/// Seen from the top, X points right and Y up. The bottom face is at z = 0
/// and the top face at z = thickness.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct BoardMass {
    /// mm³.
    pub volume: f64,
    /// g.
    pub mass: f64,
    /// mm, board frame.
    pub com: [f64; 3],
    /// About the centre of mass, board axes, g mm².
    pub inertia: [[f64; 3]; 3],
}

impl BoardMass {
    const EMPTY: Self = Self {
        volume: 0.0,
        mass: 0.0,
        com: [0.0; 3],
        inertia: [[0.0; 3]; 3],
    };

    fn add(&mut self, part: Self) {
        if part.mass == 0.0 {
            return;
        }
        let total = self.mass + part.mass;
        let d: [f64; 3] = std::array::from_fn(|i| part.com[i] - self.com[i]);
        // The two parallel-axis shifts to their shared centre combine to this
        // single shift along the separation, weighted by the reduced mass.
        let reduced_mass = self.mass * part.mass / total;
        for i in 0..3 {
            self.com[i] += d[i] * (part.mass / total);
            let a = d[(i + 1) % 3];
            let b = d[(i + 2) % 3];
            self.inertia[i][i] += part.inertia[i][i] + reduced_mass * (a * a + b * b);
            for j in 0..i {
                self.inertia[i][j] += part.inertia[i][j] - reduced_mass * (d[i] * d[j]);
                self.inertia[j][i] = self.inertia[i][j];
            }
        }
        self.mass = total;
        self.volume += part.volume;
    }
}

/// Planar area and its moments about the centroid. `ixy` is the integral of x*y.
#[derive(Default)]
struct AreaMoments {
    area: f64,
    centre: [f64; 2],
    ixx: f64,
    iyy: f64,
    ixy: f64,
}

impl AreaMoments {
    fn extrude(self, bottom: f64, height: f64, density: f64) -> BoardMass {
        if self.area == 0.0 || height <= 0.0 {
            return BoardMass::EMPTY;
        }
        let volume = self.area * height;
        let mass = volume * density;
        let surface_density = height * density;
        let thickness_moment = mass * height * height / 12.0;
        let ix = self.ixx * surface_density;
        let iy = self.iyy * surface_density;
        let xy = -self.ixy * surface_density;
        BoardMass {
            volume,
            mass,
            com: [self.centre[0], self.centre[1], bottom + height / 2.0],
            inertia: [
                [ix + thickness_moment, xy, 0.0],
                [xy, iy + thickness_moment, 0.0],
                [0.0, 0.0, ix + iy],
            ],
        }
    }
}

fn outline_moments(outline: &[Contour]) -> AreaMoments {
    let regions: Vec<_> = outline
        .iter()
        .map(|c| PolySet::from_ring_nonzero(c.to_ring(10_000)))
        .collect();
    // Take moments near the board, so translating it does not cost precision.
    let origin = outline.first().map_or(P::ZERO, |c| c.path.start);
    let local = |p: P| {
        [
            (f64::from(p.x) - f64::from(origin.x)) / NM_PER_MM,
            (f64::from(p.y) - f64::from(origin.y)) / NM_PER_MM,
        ]
    };
    let mut area = 0.0;
    let (mut mx, mut my) = (0.0, 0.0);
    let (mut ixx, mut iyy, mut ixy) = (0.0, 0.0, 0.0);
    for (i, region) in regions.iter().enumerate() {
        for ring in region.rings() {
            // Each region has normalised winding. Each enclosing contour flips its sign.
            let depth = regions
                .iter()
                .enumerate()
                .filter(|(j, other)| i != *j && other.contains(ring[0]))
                .count();
            let sign = if depth % 2 == 0 { 1.0 } else { -1.0 };
            for (&a, &b) in ring.iter().zip(ring.iter().cycle().skip(1)) {
                let [x, y] = local(a);
                let [u, v] = local(b);
                let cross = sign * orient(origin, a, b) as f64 / (NM_PER_MM * NM_PER_MM);
                area += cross / 2.0;
                mx += cross * (x + u) / 6.0;
                my += cross * (y + v) / 6.0;
                ixx += cross * (y * y + y * v + v * v) / 12.0;
                iyy += cross * (x * x + x * u + u * u) / 12.0;
                ixy += cross * (2.0 * x * y + x * v + u * y + 2.0 * u * v) / 24.0;
            }
        }
    }
    if area <= 0.0 {
        return AreaMoments::default();
    }
    let (cx, cy) = (mx / area, my / area);
    AreaMoments {
        area,
        centre: [to_mm(origin.x) + cx, to_mm(origin.y) + cy],
        ixx: ixx - my * cy,
        iyy: iyy - mx * cx,
        ixy: ixy - mx * cy,
    }
}

fn laminate_mass(mech: &BoardMech) -> BoardMass {
    outline_moments(&mech.outline).extrude(0.0, to_mm(mech.thickness), LAMINATE_DENSITY)
}

/// Mass properties of the laminate and its known component bodies. Drilled holes
/// are left filled because they typically account for under one percent of board mass.
pub fn board_mass(mech: &BoardMech) -> BoardMass {
    let mut board = laminate_mass(mech);
    for body in mech.bodies.iter().filter(|b| b.known && b.height > 0) {
        let (x, y) = (to_mm(body.min.x), to_mm(body.min.y));
        let (width, depth) = (to_mm(body.max.x) - x, to_mm(body.max.y) - y);
        if width <= 0.0 || depth <= 0.0 {
            continue;
        }
        let height = to_mm(body.height);
        let bottom = match body.side {
            Side::Top => to_mm(mech.thickness),
            Side::Bottom => -height,
        };
        let area = width * depth;
        let rect = AreaMoments {
            area,
            centre: [x + width / 2.0, y + depth / 2.0],
            ixx: area * depth * depth / 12.0,
            iyy: area * width * width / 12.0,
            ixy: 0.0,
        };
        board.add(rect.extrude(bottom, height, BODY_DENSITY));
    }
    board
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board_mech;
    use fab_elec_fixtures::ledctl;

    #[test]
    fn board_mass_and_cutout_moments() {
        let design = ledctl();
        let unit = &design.units[0];
        let board = unit.board.as_deref().expect("fixture has a board");
        let mech = board_mech(board, &unit.circuit);
        let laminate = laminate_mass(&mech);
        let whole = board_mass(&mech);
        let thickness = to_mm(mech.thickness);
        // The board's own polygon operation gives an independent area at 1 µm resolution.
        let area = board.outline_polyset(1_000).area() / (NM_PER_MM * NM_PER_MM);
        let expected_mass = area * thickness * LAMINATE_DENSITY;
        assert!(
            (laminate.mass - expected_mass).abs() <= 0.005 * expected_mass,
            "laminate mass {} g, expected {expected_mass} g",
            laminate.mass
        );
        assert!(
            whole.mass > laminate.mass,
            "populated board {} g must outweigh laminate {} g",
            whole.mass,
            laminate.mass
        );
        let bounds = board.outline_bbox();
        let [x, y, z] = whole.com;
        assert!(
            (to_mm(bounds.min.x)..=to_mm(bounds.max.x)).contains(&x),
            "centre x {x} mm outside board bounds"
        );
        assert!(
            (to_mm(bounds.min.y)..=to_mm(bounds.max.y)).contains(&y),
            "centre y {y} mm outside board bounds"
        );
        assert!(
            z > thickness / 2.0,
            "populated centre z {z} mm must be above the mid-plane at {} mm",
            thickness / 2.0
        );
        for i in 0..3 {
            assert!(
                whole.inertia[i][i] > 0.0,
                "inertia diagonal {i} must be positive: {}",
                whole.inertia[i][i]
            );
            for j in 0..i {
                assert_eq!(
                    whole.inertia[i][j], whole.inertia[j][i],
                    "inertia must be symmetric at ({i}, {j})"
                );
            }
        }

        // Both contours have the same winding, so only nesting makes the inner one a cutout.
        let outer = Contour::rect(P::ZERO, P::mm(40.0, 74.0), 0);
        let cutout = Contour::rect(P::mm(15.0, 32.0), P::mm(25.0, 42.0), 0);
        let section = outline_moments(&[outer, cutout]);
        assert!(
            (section.area - 2860.0).abs() < 1e-9,
            "cutout must leave 2860 mm², got {}",
            section.area
        );
        assert!(
            (section.centre[0] - 20.0).abs() < 1e-9 && (section.centre[1] - 37.0).abs() < 1e-9,
            "centred cutout must preserve centroid [20, 37] mm, got {:?}",
            section.centre
        );
        let ixx = (40.0 * 74.0_f64.powi(3) - 10.0 * 10.0_f64.powi(3)) / 12.0;
        assert!(
            (section.ixx - ixx).abs() <= 1e-6 * ixx,
            "cutout Ixx about centroid {} mm⁴, expected {ixx} mm⁴",
            section.ixx
        );
    }
}

//! Unevaluated CSG instructions for panel parts. Units are millimetres; z points up
//! along the mould's pull direction. Each generator chooses its own origin.

use std::{f64::consts::PI, sync::Arc};

pub type V2 = [f64; 2];
pub type V3 = [f64; 3];

/// A closed planar outline, star-shaped about its local origin: each ray from
/// the origin crosses the boundary once, so a centre fan triangulates the cap.
/// Points run counter-clockwise seen from the ring's local +z side.
/// The origin is `(cx, cy, z)`, tilted about x in degrees; positive tilt lifts +y.
/// Loft centres must have strictly increasing z, and the rings must not cross.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ring {
    pub z: f64,
    pub pts: Vec<V2>,
    pub cx: f64,
    pub cy: f64,
    pub tilt: f64,
}

impl Ring {
    /// Whether the ring is level and centred on the z axis.
    pub fn flat(&self) -> bool {
        self.cx == 0.0 && self.cy == 0.0 && self.tilt == 0.0
    }
}

/// The world position of a point in a ring's plane.
pub fn ring_point(ring: &Ring, point: V2) -> V3 {
    if ring.tilt == 0.0 {
        return [ring.cx + point[0], ring.cy + point[1], ring.z];
    }
    let a = ring.tilt * PI / 180.0;
    let c = a.cos();
    let s = a.sin();
    [
        ring.cx + point[0],
        ring.cy + point[1] * c,
        ring.z + point[1] * s,
    ]
}

/// A primitive's geometry, without evaluation or meshing.
#[derive(Clone, Debug, PartialEq)]
pub enum Shape {
    /// A solid skinned through rings with equal point counts, closed by planar
    /// caps at the first and last ring. Ring centres have strictly increasing z.
    Loft {
        rings: Vec<Ring>,
        /// Empty or one flag per ring. A true flag marks a sharp edge, such as a
        /// chamfer or step, where exact surfaces split instead of smoothing.
        crease: Vec<bool>,
        /// Two level rings with the same outline and centre, making straight
        /// walls. Mesh consumers use an ordinary loft. STEP extrudes the outline,
        /// splitting turns over 25 degrees into lines and interpolated runs so
        /// notches and other sharp corners stay sharp.
        prism: bool,
    },
    /// A box with axis-aligned size, centred on `center`, rotated about z and
    /// then x through that centre in degrees. Positive x rotation lifts +y.
    /// Tapers widen the top face to
    /// `(size.x + taper_x, size.y + taper_y)`; `shift_x` moves it in local x.
    /// Drafted ribs and other non-plain boxes use `box_corners` for their geometry.
    Box {
        center: V3,
        size: V3,
        rot_z: f64,
        rot_x: f64,
        taper_x: f64,
        taper_y: f64,
        shift_x: f64,
    },
    /// A cylinder with its base centre at `center`, radius `r`, and `length`
    /// along a unit `axis`. The usual axis is +z, `[0, 0, 1]`.
    Cylinder {
        center: V3,
        r: f64,
        length: f64,
        axis: V3,
    },
    /// A sphere centred on `center`, with radius `r`.
    Sphere { center: V3, r: f64 },
    /// The convex hull of the points, allowing faces at arbitrary angles for
    /// drafted legs, snap hooks and shut-off windows. Points should be hull
    /// vertices; extra interior points are harmless.
    Hull { pts: Vec<V3> },
    /// The solid of a nested program, with its own resolution and STEP curve
    /// settings. This allows operations such as subtracting a difference.
    Group(Arc<Csg>),
    /// Simple, non-crossing contours extruded upwards from `z` by `height`.
    /// Outlines run counter-clockwise seen from +z; holes run clockwise.
    /// Unlike loft rings, these outlines need not be star-shaped.
    Extrude {
        z: f64,
        height: f64,
        contours: Vec<Vec<V2>>,
    },
}

impl Shape {
    /// Whether this is a box with no taper, top shift or x rotation.
    /// Rotation about z still permits a plain box.
    pub fn plain_box(&self) -> bool {
        matches!(
            self,
            Self::Box { taper_x, taper_y, shift_x, rot_x, .. }
                if *taper_x == 0.0 && *taper_y == 0.0 && *shift_x == 0.0 && *rot_x == 0.0
        )
    }
}

/// A named primitive and the surface slot of the faces it leaves on the result.
#[derive(Clone, Debug, PartialEq)]
pub struct Prim {
    /// A descriptive name for exported objects; it does not affect geometry.
    pub label: String,
    /// Surface slot: 0 is the main surface, 1 may be frosted, and so on.
    /// It does not change the solid. Output triangles retain their originating
    /// primitive, and STEP faces carry the corresponding surface assignment.
    pub finish: u32,
    pub shape: Shape,
}

impl Prim {
    fn new(label: impl Into<String>, shape: Shape) -> Self {
        Self {
            label: label.into(),
            finish: 0,
            shape,
        }
    }

    pub fn loft(label: impl Into<String>, rings: Vec<Ring>) -> Self {
        Self::new(
            label,
            Shape::Loft {
                rings,
                crease: Vec::new(),
                prism: false,
            },
        )
    }

    pub fn cuboid(label: impl Into<String>, center: V3, size: V3) -> Self {
        Self::new(
            label,
            Shape::Box {
                center,
                size,
                rot_z: 0.0,
                rot_x: 0.0,
                taper_x: 0.0,
                taper_y: 0.0,
                shift_x: 0.0,
            },
        )
    }

    pub fn cylinder(label: impl Into<String>, center: V3, r: f64, length: f64, axis: V3) -> Self {
        Self::new(
            label,
            Shape::Cylinder {
                center,
                r,
                length,
                axis,
            },
        )
    }

    pub fn sphere(label: impl Into<String>, center: V3, r: f64) -> Self {
        Self::new(label, Shape::Sphere { center, r })
    }

    pub fn hull(label: impl Into<String>, pts: Vec<V3>) -> Self {
        Self::new(label, Shape::Hull { pts })
    }

    pub fn group(label: impl Into<String>, csg: Csg) -> Self {
        Self::new(label, Shape::Group(Arc::new(csg)))
    }

    pub fn extrude(label: impl Into<String>, z: f64, height: f64, contours: Vec<Vec<V2>>) -> Self {
        Self::new(
            label,
            Shape::Extrude {
                z,
                height,
                contours,
            },
        )
    }

    pub fn finish(mut self, slot: u32) -> Self {
        self.finish = slot;
        self
    }

    pub fn crease(mut self, flags: Vec<bool>) -> Self {
        match &mut self.shape {
            Shape::Loft { crease, .. } => *crease = flags,
            _ => debug_assert!(false, "crease requires a loft"),
        }
        self
    }

    pub fn prism(mut self) -> Self {
        match &mut self.shape {
            Shape::Loft { prism, .. } => *prism = true,
            _ => debug_assert!(false, "prism requires a loft"),
        }
        self
    }

    pub fn rot_z(mut self, deg: f64) -> Self {
        match &mut self.shape {
            Shape::Box { rot_z, .. } => *rot_z = deg,
            _ => debug_assert!(false, "rot_z requires a box"),
        }
        self
    }

    pub fn rot_x(mut self, deg: f64) -> Self {
        match &mut self.shape {
            Shape::Box { rot_x, .. } => *rot_x = deg,
            _ => debug_assert!(false, "rot_x requires a box"),
        }
        self
    }

    pub fn taper(mut self, x: f64, y: f64) -> Self {
        match &mut self.shape {
            Shape::Box {
                taper_x, taper_y, ..
            } => {
                *taper_x = x;
                *taper_y = y;
            }
            _ => debug_assert!(false, "taper requires a box"),
        }
        self
    }

    pub fn shift_x(mut self, x: f64) -> Self {
        match &mut self.shape {
            Shape::Box { shift_x, .. } => *shift_x = x,
            _ => debug_assert!(false, "shift_x requires a box"),
        }
        self
    }
}

/// Bottom corners 0..3, then top corners 4..7, each counter-clockwise from +z
/// before rotating the box about its centre.
pub fn box_corners(
    center: V3,
    size: V3,
    rot_z: f64,
    rot_x: f64,
    taper_x: f64,
    taper_y: f64,
    shift_x: f64,
) -> [V3; 8] {
    let a = rot_z * PI / 180.0;
    let ca = a.cos();
    let sa = a.sin();
    let b = rot_x * PI / 180.0;
    let cb = b.cos();
    let sb = b.sin();
    let mut corners = [[0.0; 3]; 8];
    for (k, corner) in corners.iter_mut().enumerate() {
        let top = k >= 4;
        let q = k & 3;
        let hx = (size[0] + if top { taper_x } else { 0.0 }) * 0.5;
        let hy = (size[1] + if top { taper_y } else { 0.0 }) * 0.5;
        let x = (if q == 1 || q == 2 { hx } else { -hx }) + if top { shift_x } else { 0.0 };
        let y = if q >= 2 { hy } else { -hy };
        let z = (if top { 0.5 } else { -0.5 }) * size[2];
        if rot_x == 0.0 && taper_x == 0.0 && shift_x == 0.0 {
            // This evaluation order keeps float rounding identical to the plain drafted-rib case.
            *corner = [
                center[0] + x * ca - y * sa,
                center[1] + x * sa + y * ca,
                center[2] + z,
            ];
            continue;
        }
        let xr = x * ca - y * sa;
        let yr = x * sa + y * ca;
        *corner = [
            center[0] + xr,
            center[1] + yr * cb - z * sb,
            center[2] + yr * sb + z * cb,
        ];
    }
    corners
}

/// Cylinder axis angles in degrees: tilt from +z about y, then turn about z.
/// In an x, y, z Euler rotation sequence, x is zero.
pub fn axis_angles(axis: V3) -> (f64, f64) {
    let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    let tilt_deg = if len > 0.0 {
        (axis[2] / len).min(1.0).max(-1.0).acos() * 180.0 / PI
    } else {
        0.0
    };
    let turn_deg = axis[1].atan2(axis[0]) * 180.0 / PI;
    (tilt_deg, turn_deg)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsgOp {
    Add,
    Sub,
    /// Keep only the intersection of the current result and this primitive.
    Intersect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CsgStep {
    pub op: CsgOp,
    pub prim: Prim,
}

/// Start with an empty solid and apply each step to the running result in order.
#[derive(Clone, Debug, PartialEq)]
pub struct Csg {
    pub steps: Vec<CsgStep>,
    /// Circle resolution hint for cylinders and spheres.
    pub segments: i32,
    /// STEP lofts use chord-length curve parameters, deriving ring planes and
    /// heights from interpolated poles; otherwise level rings use uniform parameters.
    pub step_chord_length: bool,
    /// STEP lofts thin longer rings to this point count. Raise it to retain all
    /// samples of detailed outlines such as knurl teeth.
    pub step_max_ring_pts: i32,
}

impl Default for Csg {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            segments: 128,
            step_chord_length: false,
            step_max_ring_pts: 256,
        }
    }
}

impl Csg {
    pub fn add(&mut self, prim: Prim) {
        self.steps.push(CsgStep {
            op: CsgOp::Add,
            prim,
        });
    }

    pub fn sub(&mut self, prim: Prim) {
        self.steps.push(CsgStep {
            op: CsgOp::Sub,
            prim,
        });
    }

    pub fn intersect(&mut self, prim: Prim) {
        self.steps.push(CsgStep {
            op: CsgOp::Intersect,
            prim,
        });
    }

    /// Move every primitive up by `dz`.
    pub fn lift(&mut self, dz: f64) {
        for step in &mut self.steps {
            match &mut step.prim.shape {
                Shape::Loft { rings, .. } => rings.iter_mut().for_each(|ring| ring.z += dz),
                Shape::Box { center, .. } | Shape::Cylinder { center, .. } | Shape::Sphere { center, .. } => center[2] += dz,
                Shape::Hull { pts } => pts.iter_mut().for_each(|p| p[2] += dz),
                Shape::Group(group) => Arc::make_mut(group).lift(dz),
                Shape::Extrude { z, .. } => *z += dz,
            }
        }
    }
}

/// A stable FNV-1a hash of primitive geometry, face slots and circle resolution.
/// Labels and STEP curve settings are excluded. Extrusion contours are also
/// absent from this field sequence; only their z and height contribute.
/// Counts use u64, enums and slots use u32, settings use i32, and flags use u8.
/// All multibyte values, including f64 bit patterns, are little-endian.
pub fn csg_hash(csg: &Csg) -> u64 {
    let mut hash = 1469598103934665603;
    hash_program(&mut hash, csg);
    hash
}

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash = (*hash ^ u64::from(*byte)).wrapping_mul(1099511628211);
    }
}

fn hash_floats(hash: &mut u64, values: &[f64]) {
    for value in values {
        hash_bytes(hash, &value.to_bits().to_le_bytes());
    }
}

fn hash_program(hash: &mut u64, csg: &Csg) {
    hash_bytes(hash, &csg.segments.to_le_bytes());
    hash_bytes(hash, &(csg.steps.len() as u64).to_le_bytes());
    for step in &csg.steps {
        let op: u32 = match step.op {
            CsgOp::Add => 0,
            CsgOp::Sub => 1,
            CsgOp::Intersect => 2,
        };
        hash_bytes(hash, &op.to_le_bytes());
        let kind: u32 = match &step.prim.shape {
            Shape::Loft { .. } => 0,
            Shape::Box { .. } => 1,
            Shape::Cylinder { .. } => 2,
            Shape::Sphere { .. } => 3,
            Shape::Hull { .. } => 4,
            Shape::Group(_) => 5,
            Shape::Extrude { .. } => 6,
        };
        hash_bytes(hash, &kind.to_le_bytes());
        if let Shape::Loft { rings, crease, .. } = &step.prim.shape {
            for ring in rings {
                hash_floats(hash, &[ring.z, ring.cx, ring.cy, ring.tilt]);
                for point in &ring.pts {
                    hash_floats(hash, point);
                }
                hash_bytes(hash, &(ring.pts.len() as u64).to_le_bytes());
            }
            for flag in crease {
                hash_bytes(hash, &[u8::from(*flag)]);
            }
        }

        // Absent fields contribute their defaults in the common field order.
        let mut center = [0.0; 3];
        let mut size = [0.0; 3];
        let mut axis = [0.0, 0.0, 1.0];
        let mut pts: &[V3] = &[];
        let mut rot_z = 0.0;
        let mut r = 0.0;
        let mut taper_y = 0.0;
        let mut taper_x = 0.0;
        let mut shift_x = 0.0;
        let mut rot_x = 0.0;
        let mut prism = false;
        match &step.prim.shape {
            Shape::Loft { prism: value, .. } => prism = *value,
            Shape::Box {
                center: c,
                size: s,
                rot_z: rz,
                rot_x: rx,
                taper_x: tx,
                taper_y: ty,
                shift_x: sx,
            } => {
                center = *c;
                size = *s;
                rot_z = *rz;
                rot_x = *rx;
                taper_x = *tx;
                taper_y = *ty;
                shift_x = *sx;
            }
            Shape::Cylinder {
                center: c,
                r: radius,
                length,
                axis: a,
            } => {
                center = *c;
                size[2] = *length;
                axis = *a;
                r = *radius;
            }
            Shape::Sphere {
                center: c,
                r: radius,
            } => {
                center = *c;
                r = *radius;
            }
            Shape::Hull { pts: points } => pts = points,
            Shape::Group(_) => {}
            Shape::Extrude { z, height, .. } => {
                center[2] = *z;
                size[2] = *height;
            }
        }
        hash_floats(hash, &center);
        hash_floats(hash, &size);
        hash_floats(hash, &axis);
        for point in pts {
            hash_floats(hash, point);
        }
        hash_bytes(hash, &(pts.len() as u64).to_le_bytes());
        hash_floats(hash, &[rot_z, r, taper_y, taper_x, shift_x, rot_x]);
        hash_bytes(hash, &step.prim.finish.to_le_bytes());
        hash_bytes(hash, &[u8::from(prism)]);
        if let Shape::Group(group) = &step.prim.shape {
            hash_program(hash, group);
        }
        // Contours are geometry too: two extrusions of different outlines must not share a hash.
        if let Shape::Extrude { contours, .. } = &step.prim.shape {
            for contour in contours {
                for point in contour {
                    hash_floats(hash, point);
                }
                hash_bytes(hash, &(contour.len() as u64).to_le_bytes());
            }
            hash_bytes(hash, &(contours.len() as u64).to_le_bytes());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_tracks_geometry_and_ignores_labels() {
        let outline = vec![[-1.0, -1.0], [1.0, -1.0], [1.0, 1.0], [-1.0, 1.0]];
        let mut csg = Csg::default();
        csg.add(
            Prim::loft(
                "body",
                vec![
                    Ring {
                        z: -1.0,
                        pts: outline.clone(),
                        ..Ring::default()
                    },
                    Ring {
                        z: 1.0,
                        pts: outline.clone(),
                        ..Ring::default()
                    },
                ],
            )
            .crease(vec![false, true])
            .prism(),
        );
        csg.add(
            Prim::cuboid("rib", [1.0, 2.0, 3.0], [4.0, 3.0, 5.0])
                .taper(2.0, 1.0)
                .shift_x(0.5)
                .rot_z(15.0)
                .rot_x(10.0)
                .finish(1),
        );
        csg.sub(Prim::cylinder(
            "bore",
            [1.0, 2.0, 3.0],
            0.5,
            4.0,
            [0.0, 0.0, 1.0],
        ));
        csg.intersect(Prim::hull(
            "clip",
            vec![
                [0.0, 0.0, 0.0],
                [2.0, 0.0, 0.0],
                [0.0, 2.0, 0.0],
                [0.0, 0.0, 2.0],
            ],
        ));
        let mut inner = Csg::default();
        inner.add(Prim::sphere("round", [0.0; 3], 1.0));
        csg.sub(Prim::group("cavity", inner));
        csg.add(Prim::extrude("mark", 1.0, 0.2, vec![outline]));
        let original_hash = csg_hash(&csg);

        fn relabel(csg: &mut Csg) {
            for (i, step) in csg.steps.iter_mut().enumerate() {
                step.prim.label = format!("renamed {i}");
                if let Shape::Group(group) = &mut step.prim.shape {
                    relabel(Arc::make_mut(group));
                }
            }
        }
        let mut renamed = csg.clone();
        relabel(&mut renamed);
        assert_eq!(
            csg_hash(&renamed),
            original_hash,
            "all labels, including nested labels, are excluded"
        );

        let changes: &[(&str, fn(&mut Csg))] = &[
            ("ring z", |c| {
                if let Shape::Loft { rings, .. } = &mut c.steps[0].prim.shape {
                    rings[0].z += 0.125;
                }
            }),
            ("ring point", |c| {
                if let Shape::Loft { rings, .. } = &mut c.steps[0].prim.shape {
                    rings[0].pts[0][1] += 0.125;
                }
            }),
            ("ring centre", |c| {
                if let Shape::Loft { rings, .. } = &mut c.steps[0].prim.shape {
                    rings[0].cx += 0.125;
                }
            }),
            ("ring tilt", |c| {
                if let Shape::Loft { rings, .. } = &mut c.steps[0].prim.shape {
                    rings[0].tilt += 0.125;
                }
            }),
            ("crease flag", |c| {
                if let Shape::Loft { crease, .. } = &mut c.steps[0].prim.shape {
                    crease[0] = true;
                }
            }),
            ("prism flag", |c| {
                if let Shape::Loft { prism, .. } = &mut c.steps[0].prim.shape {
                    *prism = false;
                }
            }),
            ("box taper", |c| {
                if let Shape::Box { taper_x, .. } = &mut c.steps[1].prim.shape {
                    *taper_x += 0.125;
                }
            }),
            ("box rotation", |c| {
                if let Shape::Box { rot_x, .. } = &mut c.steps[1].prim.shape {
                    *rot_x += 0.125;
                }
            }),
            ("cylinder axis", |c| {
                if let Shape::Cylinder { axis, .. } = &mut c.steps[2].prim.shape {
                    axis[0] += 0.125;
                }
            }),
            ("cylinder length", |c| {
                if let Shape::Cylinder { length, .. } = &mut c.steps[2].prim.shape {
                    *length += 0.125;
                }
            }),
            ("hull point", |c| {
                if let Shape::Hull { pts } = &mut c.steps[3].prim.shape {
                    pts[0][2] += 0.125;
                }
            }),
            ("finish slot", |c| c.steps[1].prim.finish += 1),
            ("nested geometry", |c| {
                if let Shape::Group(group) = &mut c.steps[4].prim.shape {
                    if let Shape::Sphere { r, .. } = &mut Arc::make_mut(group).steps[0].prim.shape {
                        *r += 0.125;
                    }
                }
            }),
            ("extrusion z", |c| {
                if let Shape::Extrude { z, .. } = &mut c.steps[5].prim.shape {
                    *z += 0.125;
                }
            }),
            ("extrusion contour", |c| {
                if let Shape::Extrude { contours, .. } = &mut c.steps[5].prim.shape {
                    contours[0][0][0] += 0.125;
                }
            }),
            ("segments", |c| c.segments += 1),
            ("operation", |c| c.steps[0].op = CsgOp::Sub),
            ("step count", |c| {
                let _ = c.steps.pop();
            }),
        ];
        for (property, change) in changes {
            let mut changed = csg.clone();
            change(&mut changed);
            assert_ne!(
                csg_hash(&changed),
                original_hash,
                "hash must change with {property}"
            );
        }
    }

    #[test]
    fn corners_ring_planes_and_prismatoid_volume() {
        let center = [1.0, 2.0, 3.0];
        let size = [4.0, 6.0, 8.0];
        let plain = box_corners(center, size, 0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!(
            plain,
            [
                [-1.0, -1.0, -1.0],
                [3.0, -1.0, -1.0],
                [3.0, 5.0, -1.0],
                [-1.0, 5.0, -1.0],
                [-1.0, -1.0, 7.0],
                [3.0, -1.0, 7.0],
                [3.0, 5.0, 7.0],
                [-1.0, 5.0, 7.0],
            ],
            "plain corners are centred, bottom then top, each counter-clockwise"
        );
        let plain_prim = Prim::cuboid("box", center, size).rot_z(30.0);
        assert!(plain_prim.shape.plain_box(), "z rotation keeps a plain box");
        assert!(
            !plain_prim.shift_x(0.5).shape.plain_box(),
            "a shifted top requires box corners"
        );

        let drafted = box_corners([0.0; 3], [4.0, 3.0, 5.0], 0.0, 0.0, 2.0, 1.0, 0.5);
        assert_eq!(
            drafted,
            [
                [-2.0, -1.5, -2.5],
                [2.0, -1.5, -2.5],
                [2.0, 1.5, -2.5],
                [-2.0, 1.5, -2.5],
                [-2.5, -2.0, 2.5],
                [3.5, -2.0, 2.5],
                [3.5, 2.0, 2.5],
                [-2.5, 2.0, 2.5],
            ],
            "taper adds to top widths and shift moves only the top in local x"
        );

        let rotated = box_corners(center, size, 0.0, 90.0, 0.0, 0.0, 0.0);
        for (i, point) in plain.iter().enumerate() {
            let expected = [
                point[0],
                center[1] - (point[2] - center[2]),
                center[2] + (point[1] - center[1]),
            ];
            for (axis, value) in expected.iter().enumerate() {
                assert!(
                    (rotated[i][axis] - value).abs() < 1e-5,
                    "x rotation maps +y to +z about the centre: corner {i}, axis {axis}"
                );
            }
        }
        let combined = box_corners([0.0; 3], [2.0, 4.0, 6.0], 90.0, 90.0, 0.0, 0.0, 0.0);
        for (axis, value) in [-2.0, -3.0, 1.0].iter().enumerate() {
            assert!(
                (combined[6][axis] - value).abs() < 1e-5,
                "box rotation applies z before x: axis {axis}"
            );
        }

        let mut ring = Ring {
            z: 3.0,
            cx: 1.0,
            cy: 2.0,
            ..Ring::default()
        };
        assert_eq!(
            ring_point(&ring, [4.0, 5.0]),
            [5.0, 7.0, 3.0],
            "untilted ring points add their centre without trigonometry"
        );
        assert!(!ring.flat(), "an offset level ring is not flat about z");
        assert!(Ring::default().flat(), "the default ring is flat about z");
        ring.tilt = 90.0;
        let lifted = ring_point(&ring, [0.0, 2.0]);
        for (axis, value) in [1.0, 2.0, 5.0].iter().enumerate() {
            assert!(
                (lifted[axis] - value).abs() < 1e-6,
                "ring tilt maps +y to +z: axis {axis}"
            );
        }
        assert_eq!(axis_angles([0.0; 3]), (0.0, 0.0), "zero axis has zero angles");
        assert_eq!(
            axis_angles([0.0, 0.0, 2.0]),
            (0.0, 0.0),
            "+z axis has zero tilt and turn regardless of length"
        );
        let (tilt, turn) = axis_angles([0.0, 3.0, 0.0]);
        assert!(
            (tilt - 90.0).abs() < 1e-5 && (turn - 90.0).abs() < 1e-5,
            "+y axis tilts about y and then turns about z"
        );

        fn volume(corners: &[V3; 8]) -> f64 {
            let faces = [
                [0, 3, 2, 1],
                [4, 5, 6, 7],
                [0, 1, 5, 4],
                [1, 2, 6, 5],
                [2, 3, 7, 6],
                [3, 0, 4, 7],
            ];
            let mut volume = 0.0;
            for face in faces {
                for triangle in [[face[0], face[1], face[2]], [face[0], face[2], face[3]]] {
                    let [a, b, c] = triangle.map(|i| corners[i]);
                    // Divergence theorem, with each triangle wound outwards.
                    volume += (a[0] * (b[1] * c[2] - b[2] * c[1])
                        + a[1] * (b[2] * c[0] - b[0] * c[2])
                        + a[2] * (b[0] * c[1] - b[1] * c[0]))
                        / 6.0;
                }
            }
            volume
        }

        // The end rectangles are not similar, so use the general prismatoid formula.
        let a1: f64 = 4.0 * 3.0;
        let a2 = 6.0 * 4.0;
        let amid = 5.0 * 3.5;
        let expected = 5.0 / 6.0 * (a1 + 4.0 * amid + a2);
        assert!(
            (volume(&drafted) - expected).abs() / expected < 1e-4,
            "drafted box volume follows the general prismatoid formula"
        );
        let unshifted = box_corners([0.0; 3], [4.0, 3.0, 5.0], 0.0, 0.0, 2.0, 1.0, 0.0);
        assert!(
            (volume(&unshifted) - volume(&drafted)).abs() / expected < 1e-4,
            "shifting the top face does not change prismatoid volume"
        );

        // Proportional taper gives similar ends, where the square-root formula applies.
        let frustum = box_corners([0.0; 3], [4.0, 3.0, 5.0], 0.0, 0.0, 2.0, 1.5, 0.5);
        let a2 = 6.0 * 4.5;
        let expected = 5.0 / 3.0 * (a1 + a2 + (a1 * a2).sqrt());
        assert!(
            (volume(&frustum) - expected).abs() / expected < 1e-4,
            "similar end faces follow the square-root frustum volume formula"
        );
    }
}

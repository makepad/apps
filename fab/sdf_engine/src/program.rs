//! A shape lowered to the engine's program: a flat stream of f32 RGBA texels
//! that GPU field evaluation and the CPU twin ([`crate::twin`]) both interpret, op
//! for op, so their results agree bit for bit.
//!
//! The stream is the node tree walked depth first:
//! - a leaf pushes its distance (and the material slot in force) on the value
//!   stack;
//! - a Combine pushes its first operand, then for each next one its program
//!   followed by a FOLD that combines the top two values (the slot rules of
//!   `csg_sdf::distance_slot`);
//! - point ops bracket their subtree: XFORM_PUSH moves the point into the
//!   child's frame and XFORM_POP restores it and scales the distance back;
//!   DEFORM_PUSH / DEFORM_POP warp it and divide by the warp's Lipschitz bound;
//! - Shell and Round act on the top value; MAT_PUSH / MAT_POP set the slot of
//!   the leaves inside (the innermost wins); a Material with a `within`
//!   region runs the region's program first and MAT_WITHIN pops its value:
//!   the slot applies where it is <= 0, elsewhere the inherited one stays;
//! - Instance becomes a sharp union of its copies (each one an XFORM bracket
//!   around the same subprogram); Bone and Id leave the field as it is;
//! - a Texture pushes its source's values at the four corners of its gradient's
//!   tetrahedron (XFORM brackets of a translation; only for its masks or the
//!   triplanar blend), the source's value, then its pattern kernel's value per
//!   projection (KERNEL ops in XFORM brackets that turn the point into the
//!   kernel's frame and units, no SCALE: a pattern is no distance); TEX
//!   combines them into the textured value.
//!
//! All parameters are converted to f32 here, once: what the GPU and the twin
//! compute from is the same bits. A program starts with one header texel
//! (op-stream end, value-stack depth, point-stack depth, 0). Payloads follow
//! the op stream and count toward `texels.len()` and buffer uploads.

use crate::kernel::KernelSlot;
use makepad_csg_sdf::{SdfDeform, SdfNode, SdfOp, SdfPlace, TexProject};

/// Op codes (the header texel's `x`).
pub mod op {
    pub const PRIM: f32 = 0.0;
    pub const XFORM_PUSH: f32 = 1.0;
    pub const XFORM_POP: f32 = 2.0;
    pub const DEFORM_PUSH: f32 = 3.0;
    pub const DEFORM_POP: f32 = 4.0;
    pub const FOLD: f32 = 5.0;
    pub const SHELL: f32 = 6.0;
    pub const ROUND: f32 = 7.0;
    pub const MAT_PUSH: f32 = 8.0;
    pub const MAT_POP: f32 = 9.0;
    pub const MAT_WITHIN: f32 = 10.0;
    /// A compiled Splash kernel: (op, slot in the field kernel table,
    /// parameter texels, parameter count), then its parameters, four per
    /// texel. It pushes its distance like a primitive. A gridded kernel's
    /// count is [`super::GRIDDED`] more, and its parameter 0 is the texel
    /// offset from this op of its grid's words (a payload; the shader's
    /// `kernel_at_fetch` adds the op's own texel).
    pub const KERNEL: f32 = 11.0;
    /// The top value multiplied by `y`.
    pub const SCALE: f32 = 12.0;
    /// Exact mesh query: y is the immutable asset binding in the field table.
    pub const MESH: f32 = 14.0;
    /// y/z: program-relative payload offset and texel count, stored as u32 bits.
    pub const LINKS: f32 = 15.0;
    /// A texture (`csg_sdf::SdfTexture::combine`): 3 texels, (op, triplanar, gradient,
    /// depth), (1 / lipschitz, 1 / (4 step), 0, 0), (top ramp lo hi, bottom ramp lo hi).
    /// An absent mask is the ramp 2..3, 1 everywhere. Pops the pattern values, source
    /// and gradient corners, and pushes the textured value with the source's slot.
    pub const TEX: f32 = 16.0;
    /// (op, its site, 0, 0): only in a [`lower_hosted`] program. The host answers this leaf at
    /// the current point and the value comes in from outside.
    pub const LEAF: f32 = 17.0;
}

/// Added to a KERNEL op's parameter count when the kernel reads a grid.
pub const GRIDDED: usize = 32;

/// The texels of the op whose header is `h` (its parameters included).
pub fn op_len(h: [f32; 4]) -> usize {
    if h[0] == op::PRIM || h[0] == op::TEX {
        3
    } else if h[0] == op::XFORM_PUSH {
        4
    } else if h[0] == op::KERNEL {
        1 + h[2] as usize
    } else {
        1
    }
}

/// Primitive kinds (a PRIM's header `y`). A PRIM is 3 texels: header
/// (op, kind, a, b) then (c, d, e, f) and (g, h, i, j).
pub mod prim {
    /// a = radius
    pub const SPHERE: f32 = 0.0;
    /// a b c = half size less the rounding, d = rounding
    pub const BOX: f32 = 1.0;
    /// a = radius, b = half height
    pub const CYLINDER: f32 = 2.0;
    /// a = bottom radius, b = top radius, c = half height
    pub const CONE: f32 = 3.0;
    /// a = ring radius, b = tube radius
    pub const TORUS: f32 = 4.0;
    /// a b c = start, d e f = end - start, g = radius
    pub const CAPSULE: f32 = 5.0;
    /// a b c = radii
    pub const ELLIPSOID: f32 = 6.0;
    /// a = 2 pi / period, b = half wall; followed by a SCALE of [`GYROID_SCALE`].
    pub const GYROID: f32 = 7.0;

    /// The gyroid G(x, y, z) = sin x cos y + sin y cos z + sin z cos x is no distance: its
    /// gradient reaches sqrt(3) (and no more: each of its three terms is a dot product of two
    /// unit vectors' parts, (cos x, -sin x).(cos y, sin z) for d/dx and so on, so the squares
    /// sum to at most cos^2 + sin^2 over the three axes). Its prim's |G(a p)| / a - b is
    /// sqrt(3)-Lipschitz; this, 1 / sqrt(3) rounded down, makes it 1-Lipschitz.
    pub const GYROID_SCALE: f32 = 0.577_350_26;
}

/// Why a shape could not be lowered.
#[derive(Clone, Debug, PartialEq)]
pub enum LowerError {
    /// A kernel or mesh the field kernel table lacks.
    Unsupported(&'static str),
    Kernel(String),
}

/// A lowered program.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Program {
    /// The texels, header first.
    pub texels: Vec<[f32; 4]>,
    /// The deepest value stack and point stack it needs.
    pub value_depth: usize,
    pub point_depth: usize,
}

/// The value stack and point stack every interpreter (shader and twin)
/// provides. Lowering rejects a deeper program before dispatch.
pub const MAX_VALUE_DEPTH: usize = 16;
pub const MAX_POINT_DEPTH: usize = 8;

impl Program {
    /// The program with its result multiplied by `s` (a field's Lipschitz scale, at the root:
    /// the values its nodes combine are untouched).
    pub fn scaled(mut self, s: f32) -> Program {
        if s != 1.0 {
            let end = self.texels[0][0] as usize;
            let mut pc = 1;
            while pc < end {
                let h = self.texels[pc];
                if h[0] == op::LINKS {
                    self.texels[pc][1] = f32::from_bits(h[1].to_bits() + 1);
                }
                if h[0] == op::KERNEL && h[3] as usize >= GRIDDED {
                    self.texels[pc + 1][0] += 1.0;
                }
                pc += op_len(h);
            }
            self.texels.insert(end, [op::SCALE, s, 0.0, 0.0]);
            self.texels[0][0] = (end + 1) as f32;
        }
        self
    }
}

struct Lower<'a> {
    kernels: &'a [KernelSlot],
    /// Multiplies every kernel's distance (1 / its Lipschitz bound).
    kernel_scale: f32,
    out: Vec<[f32; 4]>,
    payloads: Vec<(usize, Vec<[f32; 4]>)>,
    /// Gridded KERNEL ops and their grids' words.
    grids: Vec<(usize, Vec<[f32; 4]>)>,
    vdepth: usize,
    vmax: usize,
    pdepth: usize,
    pmax: usize,
    /// The node being lowered reaches the program's root only scaled (a placement's scale, the
    /// final [`Program::scaled`]): the ratio a texture there gives its value holds at the root.
    root: bool,
    /// [`lower_hosted`]: the leaves the host answers, one per [`op::LEAF`], in site order.
    hosted: Option<Vec<SdfNode>>,
}

fn f(v: f64) -> f32 {
    v as f32
}

/// The world-to-local affine of a placement (its rows: 3 x (a b c t)) and its scale.
fn affine_into(r: [[f64; 3]; 3], at: [f64; 3], scale: f64) -> [[f32; 4]; 3] {
    // local = R^T (p - at) / scale
    std::array::from_fn(|i| {
        let a = [r[0][i] / scale, r[1][i] / scale, r[2][i] / scale];
        let t = -(a[0] * at[0] + a[1] * at[1] + a[2] * at[2]);
        [f(a[0]), f(a[1]), f(a[2]), f(t)]
    })
}

/// The rotation of `csg_sdf`'s Transform: about X, then Y, then Z (degrees).
fn rot_deg(deg: [f64; 3]) -> [[f64; 3]; 3] {
    let (sx, cx) = deg[0].to_radians().sin_cos();
    let (sy, cy) = deg[1].to_radians().sin_cos();
    let (sz, cz) = deg[2].to_radians().sin_cos();
    let rx = [[1.0, 0.0, 0.0], [0.0, cx, -sx], [0.0, sx, cx]];
    let ry = [[cy, 0.0, sy], [0.0, 1.0, 0.0], [-sy, 0.0, cy]];
    let rz = [[cz, -sz, 0.0], [sz, cz, 0.0], [0.0, 0.0, 1.0]];
    let m = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| -> [[f64; 3]; 3] { std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum())) };
    m(rz, m(ry, rx))
}

/// The rotation matrix of a unit quaternion (x y z w).
fn rot_quat(q: [f64; 4]) -> [[f64; 3]; 3] {
    let [x, y, z, w] = q;
    [
        [1.0 - 2.0 * (y * y + z * z), 2.0 * (x * y - z * w), 2.0 * (x * z + y * w)],
        [2.0 * (x * y + z * w), 1.0 - 2.0 * (x * x + z * z), 2.0 * (y * z - x * w)],
        [2.0 * (x * z - y * w), 2.0 * (y * z + x * w), 1.0 - 2.0 * (x * x + y * y)],
    ]
}

impl Lower<'_> {
    fn push_value(&mut self) {
        self.vdepth += 1;
        self.vmax = self.vmax.max(self.vdepth);
    }

    fn prim(&mut self, kind: f32, p: [f32; 10]) {
        self.out.push([op::PRIM, kind, p[0], p[1]]);
        self.out.push([p[2], p[3], p[4], p[5]]);
        self.out.push([p[6], p[7], p[8], p[9]]);
        self.push_value();
    }

    /// A KERNEL op: the kernel in `slot` at the current point, its parameters inline.
    fn kernel_op(&mut self, slot: usize, params: &[f32]) {
        let texels = params.len().div_ceil(4);
        self.out.push([op::KERNEL, slot as f32, texels as f32, params.len() as f32]);
        for c in params.chunks(4) {
            let mut t = [0.0f32; 4];
            t[..c.len()].copy_from_slice(c);
            self.out.push(t);
        }
        self.push_value();
    }

    fn xform(&mut self, rows: [[f32; 4]; 3], scale: f64, body: impl FnOnce(&mut Self) -> Result<(), LowerError>) -> Result<(), LowerError> {
        self.out.push([op::XFORM_PUSH, f(scale), 0.0, 0.0]);
        self.out.extend(rows);
        self.pdepth += 1;
        self.pmax = self.pmax.max(self.pdepth);
        body(self)?;
        self.pdepth -= 1;
        self.out.push([op::XFORM_POP, f(scale), 0.0, 0.0]);
        Ok(())
    }

    fn place(&mut self, pl: &SdfPlace, of: &SdfNode) -> Result<(), LowerError> {
        self.xform(affine_into(rot_quat(pl.rotate), pl.at, pl.scale), pl.scale, |s| s.node(of))
    }

    /// A leaf the host answers at the current point.
    fn host(&mut self, n: SdfNode) {
        let leaves = self.hosted.as_mut().expect("a hosted program");
        self.out.push([op::LEAF, leaves.len() as f32, 0.0, 0.0]);
        leaves.push(n);
        self.push_value();
    }

    fn node(&mut self, n: &SdfNode) -> Result<(), LowerError> {
        if self.hosted.is_some() {
            let answered = match n {
                SdfNode::Mesh(_) | SdfNode::Links(_) | SdfNode::Tubes(_) | SdfNode::Sketch(_) => true,
                SdfNode::Sculpt { strokes, .. } => !strokes.is_empty(),
                SdfNode::Kernel(k) => !self.kernels.iter().any(|s| s.matches(k)),
                _ => false,
            };
            if answered {
                self.root = false;
                self.host(n.clone());
                return Ok(());
            }
        }
        let z = 0.0f32;
        let root = self.root;
        // what passes its one child's value on, scaled at most (a culled union may be left with one)
        let through = match n {
            SdfNode::Transform { .. } | SdfNode::Bone { .. } | SdfNode::Id { .. } | SdfNode::Material { .. } => true,
            SdfNode::Deform { deform: SdfDeform::Stretch { .. }, .. } => true,
            SdfNode::Combine { of, .. } => of.len() == 1,
            _ => false,
        };
        self.root = root && through;
        match n {
            SdfNode::Sphere { radius } => self.prim(prim::SPHERE, [f(*radius), z, z, z, z, z, z, z, z, z]),
            SdfNode::Box { size, radius } => self.prim(prim::BOX, [f(size[0] * 0.5 - radius), f(size[1] * 0.5 - radius), f(size[2] * 0.5 - radius), f(*radius), z, z, z, z, z, z]),
            SdfNode::Cylinder { radius, top_radius, height } => {
                if radius == top_radius {
                    self.prim(prim::CYLINDER, [f(*radius), f(height * 0.5), z, z, z, z, z, z, z, z])
                } else {
                    self.prim(prim::CONE, [f(*radius), f(*top_radius), f(height * 0.5), z, z, z, z, z, z, z])
                }
            }
            SdfNode::Torus { radius, tube } => self.prim(prim::TORUS, [f(*radius), f(*tube), z, z, z, z, z, z, z, z]),
            // A capsule with no length (a one-point stroke) divides 0 by 0 in the program: a sphere.
            SdfNode::Capsule { a, b, radius } if (0..3).map(|k| (b[k] - a[k]) * (b[k] - a[k])).sum::<f64>() <= 1e-18 => {
                let of = Box::new(SdfNode::Sphere { radius: *radius });
                self.node(&SdfNode::Transform { at: *a, rotate: [0.0; 3], scale: 1.0, of })?;
            }
            SdfNode::Capsule { a, b, radius } => {
                self.prim(prim::CAPSULE, [f(a[0]), f(a[1]), f(a[2]), f(b[0] - a[0]), f(b[1] - a[1]), f(b[2] - a[2]), f(*radius), z, z, z])
            }
            SdfNode::Ellipsoid { radii } => self.prim(prim::ELLIPSOID, [f(radii[0]), f(radii[1]), f(radii[2]), z, z, z, z, z, z, z]),
            SdfNode::Gyroid { period, wall } => {
                self.prim(prim::GYROID, [f(std::f64::consts::TAU / period), f(wall * 0.5), z, z, z, z, z, z, z, z]);
                // a host's value stays the node's: it takes out its whole field's slope itself
                if self.hosted.is_none() {
                    self.out.push([op::SCALE, prim::GYROID_SCALE, 0.0, 0.0]);
                }
            }
            SdfNode::Combine { op: o, blend, of } => {
                self.node(&of[0])?;
                let code = match o {
                    SdfOp::Union => 0.0,
                    SdfOp::Difference => 1.0,
                    SdfOp::Intersection => 2.0,
                };
                for c in &of[1..] {
                    self.node(c)?;
                    self.out.push([op::FOLD, code, f(*blend), 0.0]);
                    self.vdepth -= 1;
                }
            }
            SdfNode::Shell { thickness, of } => {
                self.node(of)?;
                self.out.push([op::SHELL, f(*thickness), 0.0, 0.0]);
            }
            SdfNode::Round { radius, of } => {
                self.node(of)?;
                self.out.push([op::ROUND, f(*radius), 0.0, 0.0]);
            }
            SdfNode::Transform { at, rotate, scale, of } => self.xform(affine_into(rot_deg(*rotate), *at, *scale), *scale, |s| s.node(of))?,
            SdfNode::Material { slot, within, of } => {
                match within {
                    Some(w) => {
                        // a region's value is only read for its sign
                        let root = std::mem::replace(&mut self.root, false);
                        self.node(w)?;
                        self.root = root;
                        self.out.push([op::MAT_WITHIN, *slot as f32, 0.0, 0.0]);
                        self.vdepth -= 1;
                    }
                    None => self.out.push([op::MAT_PUSH, *slot as f32, 0.0, 0.0]),
                }
                self.node(of)?;
                self.out.push([op::MAT_POP, 0.0, 0.0, 0.0]);
            }
            SdfNode::Instance { places, of } => {
                for (i, pl) in places.iter().enumerate() {
                    self.place(pl, of)?;
                    if i > 0 {
                        self.out.push([op::FOLD, 0.0, 0.0, 0.0]);
                        self.vdepth -= 1;
                    }
                }
            }
            SdfNode::Bone { of, .. } | SdfNode::Id { of, .. } => self.node(of)?,
            SdfNode::Tubes(_) => return Err(LowerError::Unsupported("tubes, which only the host answers")),
            SdfNode::Sketch(_) => return Err(LowerError::Unsupported("sketches, which only the host answers")),
            SdfNode::Links(l) => {
                let data = links_data(l);
                self.payloads.push((self.out.len(), data));
                self.out.push([op::LINKS, 0.0, 0.0, 0.0]);
                self.push_value();
            }
            // the capsules of its strokes (a brick's program holds only the ones near it)
            SdfNode::Sculpt { strokes, of } => {
                // hosted, only one with no strokes gets here
                if self.hosted.is_some() {
                    return self.node(of);
                }
                let all = ([f64::NEG_INFINITY; 3], [f64::INFINITY; 3]);
                match makepad_csg_sdf::bounds::sculpt_in(strokes, of, &all) {
                    Some(n) => self.node(&n)?,
                    None => return Err(LowerError::Unsupported("a sculpt with nothing left of its solid")),
                }
            }
            SdfNode::Texture { tex, of } => {
                let slot = self.kernels.iter().position(|s| s.matches(&tex.kernel));
                if slot.is_none() && self.hosted.is_none() {
                    return Err(LowerError::Unsupported("a texture's kernel missing from the field kernel table"));
                }
                let grad = tex.needs_gradient();
                let triplanar = tex.project == TexProject::Triplanar;
                if grad {
                    for k in makepad_csg_sdf::texture::TETRA {
                        let t = k.map(|c| f(c * tex.step));
                        self.xform([[1.0, 0.0, 0.0, t[0]], [0.0, 1.0, 0.0, t[1]], [0.0, 0.0, 1.0, t[2]]], 1.0, |s| s.node(of))?;
                    }
                }
                self.node(of)?;
                let axes: &[u8] = match tex.project {
                    TexProject::Axis(a) => &[0, 1, 2][a.min(2) as usize..a.min(2) as usize + 1],
                    TexProject::Triplanar => &[0, 1, 2],
                };
                let params = tex.kernel.splash.as_ref().map(|s| s.params()).unwrap_or_default();
                let k = f(tex.kscale);
                for &a in axes {
                    // the kernel's point: the swizzle that makes axis `a` its Y, in its units
                    let rows = match a {
                        0 => [[0.0, 0.0, k, 0.0], [k, 0.0, 0.0, 0.0], [0.0, k, 0.0, 0.0]],
                        2 => [[k, 0.0, 0.0, 0.0], [0.0, 0.0, k, 0.0], [0.0, k, 0.0, 0.0]],
                        _ => [[k, 0.0, 0.0, 0.0], [0.0, k, 0.0, 0.0], [0.0, 0.0, k, 0.0]],
                    };
                    self.xform(rows, 1.0, |s| {
                        match slot {
                            Some(slot) => s.kernel_op(slot, &params),
                            None => s.host(SdfNode::Kernel(tex.kernel.clone())),
                        }
                        Ok(())
                    })?;
                }
                let none = (2.0, 3.0);
                let (top, bottom) = (tex.top.unwrap_or(none), tex.bottom.unwrap_or(none));
                self.out.push([op::TEX, if triplanar { 1.0 } else { 0.0 }, if grad { 1.0 } else { 0.0 }, f(tex.depth)]);
                // A walk's step on a host: outside, the source less the most the texture can raise
                // it bounds the distance to the displaced surface, which is what the division by
                // the texture's whole Lipschitz bound shrinks away. It needs the displacement's
                // range (the pattern known to stay in 0..1); a blend, a shell or a round above it
                // read the value, so the value stays the field's and only a texture on the root
                // path says so. The host scales its whole field by its steepest slope, so an
                // estimate below is covered.
                let steps = root && tex.displacement_range().is_some() && self.hosted.is_some();
                self.out.push([f(1.0 / tex.lipschitz), f(0.25 / tex.step), 0.0, if steps { 1.0 } else { 0.0 }]);
                self.out.push([f(top.0), f(top.1), f(bottom.0), f(bottom.1)]);
                self.vdepth -= axes.len() + if grad { 4 } else { 0 };
            }
            SdfNode::Deform { deform, lipschitz, of } => {
                let (kind, rate, axis) = match *deform {
                    SdfDeform::Twist { rate } => (0.0, rate, 0),
                    SdfDeform::Bend { rate } => (1.0, rate, 0),
                    SdfDeform::Taper { rate } => (2.0, rate, 0),
                    SdfDeform::Mirror { axis } => (3.0, 0.0, axis.min(2)),
                    // a stretch along the node's axes is an affine point map: a transform
                    // bracket whose pop divides by the warp's bound
                    SdfDeform::Stretch { scale } => {
                        let rows = std::array::from_fn(|i| std::array::from_fn(|j| if j == i { f(1.0 / scale[i]) } else { 0.0 }));
                        return self.xform(rows, 1.0 / lipschitz, |s| s.node(of));
                    }
                };
                self.out.push([op::DEFORM_PUSH, kind, f(rate), axis as f32]);
                self.pdepth += 1;
                self.pmax = self.pmax.max(self.pdepth);
                self.node(of)?;
                self.pdepth -= 1;
                self.out.push([op::DEFORM_POP, f(*lipschitz), 0.0, 0.0]);
            }
            SdfNode::Kernel(k) => {
                let Some(slot) = self.kernels.iter().position(|s| s.matches(k)) else {
                    return Err(LowerError::Unsupported("a kernel missing from the field kernel table"));
                };
                let params = k.splash.as_ref().map(|s| s.params()).unwrap_or_default();
                let at = self.out.len();
                self.kernel_op(slot, &params);
                if let Some(g) = k.splash.as_ref().and_then(|s| s.grid()) {
                    self.out[at][3] += GRIDDED as f32;
                    let mut words = g.words();
                    words.resize(words.len().div_ceil(4) * 4, 0.0);
                    self.grids.push((at, words.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()));
                }
                if self.kernel_scale != 1.0 {
                    self.out.push([op::SCALE, self.kernel_scale, 0.0, 0.0]);
                }
            }
            // A mesh leaf evaluates the exact query used by layer tracing.
            SdfNode::Mesh(m) => {
                let Some(slot) = self.kernels.iter().position(|s| s.key == m.key() && s.is_mesh()) else {
                    return Err(LowerError::Unsupported("a mesh missing from the field kernel table"));
                };
                self.out.push([op::MESH, slot as f32, 0.0, 0.0]);
                self.push_value();
                if self.kernel_scale != 1.0 {
                    self.out.push([op::SCALE, self.kernel_scale, 0.0, 0.0]);
                }
            }
        }
        Ok(())
    }
}

/// Lowers a node (usually one already culled to a brick's region) that
/// calls no kernel.
pub fn lower(n: &SdfNode) -> Result<Program, LowerError> {
    lower_with(n, &[], 1.0)
}

/// Lowers a node whose kernels are in `kernels` (the field table: a
/// kernel's slot there is its index), each kernel's distance multiplied by
/// `kernel_scale`.
pub fn lower_with(n: &SdfNode, kernels: &[KernelSlot], kernel_scale: f32) -> Result<Program, LowerError> {
    let mut l = Lower { kernels, kernel_scale, out: vec![[0.0; 4]], payloads: Vec::new(), grids: Vec::new(), vdepth: 0, vmax: 0, pdepth: 0, pmax: 0, root: true, hosted: None };
    l.node(n)?;
    if l.vmax > MAX_VALUE_DEPTH || l.pmax > MAX_POINT_DEPTH {
        return Err(LowerError::Unsupported("program exceeds GPU interpreter stack depth"));
    }
    Ok(l.finish())
}

/// Lowers a node for a host that compiles the program for the CPU: no GPU reads it, so no GPU
/// limit applies, and any node at all lowers. What has no op here (a mesh, links, a sculpt with
/// strokes, a kernel not in `kernels`) becomes an [`op::LEAF`], and the leaves come back in site
/// order for the host to answer.
pub fn lower_hosted(n: &SdfNode, kernels: &[KernelSlot]) -> Result<(Program, Vec<SdfNode>), LowerError> {
    let mut l = Lower { kernels, kernel_scale: 1.0, out: vec![[0.0; 4]], payloads: Vec::new(), grids: Vec::new(), vdepth: 0, vmax: 0, pdepth: 0, pmax: 0, root: true, hosted: Some(Vec::new()) };
    l.node(n)?;
    let leaves = l.hosted.take().unwrap_or_default();
    Ok((l.finish(), leaves))
}

impl Lower<'_> {
    /// The header, and the payloads after the ops.
    fn finish(mut self) -> Program {
        let len = self.out.len();
        self.out[0] = [len as f32, self.vmax as f32, self.pmax as f32, 0.0];
        for (pc, data) in std::mem::take(&mut self.payloads) {
            self.out[pc][1] = f32::from_bits(u32::try_from(self.out.len()).expect("program offset exceeds u32"));
            self.out[pc][2] = f32::from_bits(u32::try_from(data.len()).expect("links payload exceeds u32"));
            self.out.extend(data);
        }
        for (pc, data) in std::mem::take(&mut self.grids) {
            self.out[pc + 1][0] = (self.out.len() - pc) as f32;
            self.out.extend(data);
        }
        Program { texels: self.out, value_depth: self.vmax, point_depth: self.pmax }
    }
}

// LINKS payload (vec4s): radius/cell/clip-present/max cell count; clip plane;
// min; max; origin; dims; starts/cell-segs/segments offsets (scalar words).
// Arrays are packed four u32 bit patterns per texel. A segment is seven words:
// a.xyz, b.xyz, loop material slot. All offsets are relative to this payload.
fn links_data(l: &makepad_csg_sdf::Links) -> Vec<[f32; 4]> {
    let (cell, origin, dims, starts, cell_segs, segs) = l.grid();
    let bits = |n: usize| f32::from_bits(u32::try_from(n).expect("links index exceeds u32"));
    let (normal, off) = l.clip.unwrap_or(([0.0; 3], 0.0));
    let mut words = vec![
        f(l.radius), f(cell), if l.clip.is_some() { 1.0 } else { 0.0 },
        f32::from_bits(starts.windows(2).map(|s| s[1] - s[0]).max().unwrap_or(0)),
        f(normal[0]), f(normal[1]), f(normal[2]), f(off),
        f(l.min[0]), f(l.min[1]), f(l.min[2]), 0.0,
        f(l.max[0]), f(l.max[1]), f(l.max[2]), 0.0,
        f(origin[0]), f(origin[1]), f(origin[2]), 0.0,
        bits(dims[0]), bits(dims[1]), bits(dims[2]), 0.0,
        bits(28), bits(28 + starts.len()), bits(28 + starts.len() + cell_segs.len()), 0.0,
    ];
    words.extend(starts.iter().chain(cell_segs).map(|v| f32::from_bits(*v)));
    for (a, b, li) in segs {
        words.extend(a.iter().chain(b).map(|v| f(*v)));
        words.push(f32::from_bits(l.loops[*li as usize].slot as u32));
    }
    words.resize(words.len().div_ceil(4) * 4, 0.0);
    words.chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect()
}

#[cfg(test)]
mod tests {
    use super::prim;

    fn psc_error(x: f64) -> f64 {
        x * 2f64.powi(-22) + 3.0e-7
    }

    /// The sampled check of the gyroid's declaration: its lowered program, run by the twin (the
    /// shader's arithmetic), is never steeper than 1 by central differences over a period
    /// (refusing past 1.001: f32 noise over the differences' span stays under 2e-4).
    #[test]
    fn the_lowered_gyroid_is_one_lipschitz() {
        for (period, wall) in [(8.0, 1.2), (1.5, 0.3)] {
            let p = super::lower_with(&super::SdfNode::Gyroid { period, wall }, &[], 1.0).expect("lowers");
            let (n, h) = (24, period as f32 / 4096.0);
            let mut steepest = 0.0f32;
            for i in 0..n * n * n {
                let q = [i % n, (i / n) % n, i / (n * n)].map(|k| (k as f32 + 0.37) * period as f32 / n as f32);
                let mut g = 0.0f32;
                for axis in 0..3 {
                    let (mut a, mut b) = (q, q);
                    a[axis] += h;
                    b[axis] -= h;
                    g += ((crate::twin::eval(&p.texels, a).0 - crate::twin::eval(&p.texels, b).0) / (a[axis] - b[axis])).powi(2);
                }
                steepest = steepest.max(g.sqrt());
            }
            assert!(steepest <= 1.001 && steepest > 0.9, "the lowered gyroid (period {period}) is {steepest} steep");
        }
    }

    /// 1 / sqrt(3) rounded down, never up.
    #[test]
    fn the_gyroid_scale_rounds_down() {
        assert!((prim::GYROID_SCALE as f64) <= 1.0 / 3f64.sqrt());
        assert!((prim::GYROID_SCALE as f64) > 1.0 / 3f64.sqrt() - 1e-7);
    }

    /// psc (the shaders' sin and cos, `twin::psin_pcos` bit for bit) within
    /// `psc_error` less the argument's own rounding: a dense sweep of f32 arguments
    /// at each scale (every f32 near the reduction's quadrant edges, where it is worst, and
    /// evenly spaced ones between), against f64 sin and cos of the same f32.
    #[test]
    fn psc_stays_within_its_error() {
        for &x_max in &[1.0f64, 10.0, 100.0, 1000.0, 10000.0] {
            let mut worst = 0.0f64;
            // the argument's own rounding is the bound's other x 2^-24
            let allowed = psc_error(x_max) - x_max * 2f64.powi(-24);
            let mut check = |x: f32| {
                let (s, c) = crate::twin::psin_pcos(x);
                let (ts, tc) = (x as f64).sin_cos();
                let e = (s as f64 - ts).abs().max((c as f64 - tc).abs());
                worst = worst.max(e / allowed);
                assert!(e <= allowed, "psc({x}) is off by {e:e}, over {allowed:e}");
            };
            let n = 200_000;
            for i in 0..=n {
                let x = (-x_max + 2.0 * x_max * i as f64 / n as f64) as f32;
                check(x);
            }
            // each quadrant edge q pi/4 (the reduction's rounding of x 0.6366 + 0.5) and its
            // neighbouring f32s
            let edges = (x_max / std::f64::consts::FRAC_PI_4) as i64;
            for q in (-edges..=edges).step_by((edges as usize / 2000).max(1)) {
                let mut x = (q as f64 * std::f64::consts::FRAC_PI_4) as f32;
                for _ in 0..8 { x = f32::from_bits(x.to_bits().wrapping_sub(1)); }
                for _ in 0..16 {
                    if (x as f64).abs() <= x_max { check(x); }
                    x = f32::from_bits(x.to_bits().wrapping_add(1));
                }
            }
            println!("psc within {x_max}: worst error {worst:.3} of its bound");
        }
    }

}

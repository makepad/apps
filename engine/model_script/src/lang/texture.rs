//! `m.texture`: a field solid's surface displaced by a pattern ([`csg_sdf::SdfTexture`]); and
//! `m.voronoi`, a solid's skin as a web of struts, from the same library's cell kernels.
//!
//! The pattern and its mapping are the texture library's Splash fns (`texture.splash`): the
//! lowering derives the mapping's numbers (the seam rule's whole counts round a cylinder, the
//! cells per metre, the radial fade) and the bound on the pattern's slope, calls the library's
//! mapping with the pattern and those numbers, and compiles the fn it returns as the texture's
//! kernel. Every number the kernel reads is one of its parameters, so another pitch or depth is
//! the same program with other values.

use super::vm::lut_id;
use super::{csg_sdf, voronoi, Diag, Fields, Lower, Res, P3, V};
use csg_sdf::texture::{angle_mask, SdfTexture, TexProject};
use csg_sdf::{SdfKernel, SdfSplashExpr, SplashPoint};
use makepad_csg_math::Vec3d;
use makepad_script::{NoTrap, ScriptObject, ScriptValue};
use std::f64::consts::{FRAC_PI_2, TAU};

/// How deep under its surface a solid goes, by its own distance (the number its skin and foam are
/// cut by): the deepest of about 4k points over its box, then walked down in halving steps, since
/// that lattice steps over the middle of a wall about as thick as its spacing.
fn source_depth(of: &csg_sdf::SdfNode, bounds: (P3, P3)) -> f64 {
    const POINTS: f64 = 4096.0;
    let span: [f64; 3] = std::array::from_fn(|a| bounds.1[a] - bounds.0[a]);
    let mut h = (span[0] * span[1] * span[2] / POINTS).cbrt().max(span[0].max(span[1]).max(span[2]) / POINTS);
    // an axis thinner than the spacing still takes one point
    let n = loop {
        let n = span.map(|s| (s / h).ceil().max(1.0));
        if n[0] * n[1] * n[2] <= 1.25 * POINTS {
            break n.map(|c| c as usize);
        }
        h *= 1.1;
    };
    let mut ps = Vec::with_capacity(n[0] * n[1] * n[2]);
    for z in 0..n[2] {
        for y in 0..n[1] {
            for x in 0..n[0] {
                let at = [x, y, z].map(|i| i as f64 + 0.5);
                ps.push(Vec3d::new(bounds.0[0] + at[0] * span[0] / n[0] as f64, bounds.0[1] + at[1] * span[1] / n[1] as f64, bounds.0[2] + at[2] * span[2] / n[2] as f64));
            }
        }
    }
    let mut d = vec![0.0; ps.len()];
    csg_sdf::distance_many(of, &ps, &mut d);
    let (mut best, mut deepest) = ps.iter().zip(&d).fold((ps[0], f64::INFINITY), |b, (p, d)| if *d < b.1 { (*p, *d) } else { b });
    let mut d = [0.0; 26];
    for halving in 1..=4 {
        let step = h / (1 << halving) as f64;
        let around: [Vec3d; 26] = std::array::from_fn(|i| {
            let k = if i < 13 { i } else { i + 1 };
            best + Vec3d::new((k % 3) as f64 - 1.0, (k / 3 % 3) as f64 - 1.0, (k / 9) as f64 - 1.0) * step
        });
        csg_sdf::distance_many(of, &around, &mut d);
        for (p, d) in around.iter().zip(&d) {
            if *d < deepest {
                (best, deepest) = (*p, *d);
            }
        }
    }
    -deepest
}

/// A pattern ready to map.
struct Pat {
    /// Its kernel fn `fn(c)`, the point in cells.
    func: ScriptValue,
    /// One cell along u, v and w (the height above the surface), in kernel units.
    cell: [f64; 3],
    /// The most the value changes per cell along each of u, v and w.
    slope: [f64; 3],
    feature: Feature,
    /// Its value stays in 0..1 by its construction (`SdfTexture::unit_pattern`): the library's
    /// patterns do (`the_library_patterns_stay_in_0_to_1`), a user's `Pattern{}` is only asked to.
    unit: bool,
}

/// How the narrowest feature the pattern prints follows from the mapped cells.
enum Feature {
    /// Grooves across u (both: and across v): half their spacing.
    Grooves { both: bool },
    /// This long (kernel units).
    Fixed(f64),
}

/// The point `p` with axis `a` as its Y (the kernel's frame for that axis).
fn swizzle(a: u8, p: P3) -> P3 {
    match a {
        0 => [p[2], p[0], p[1]],
        2 => [p[0], p[2], p[1]],
        _ => p,
    }
}

impl Lower<'_, '_, '_> {
    /// The texture of `f` (a `Texture` part) over a source in `bounds` (kernel units).
    pub(super) fn texture(&mut self, f: &Fields, bounds: (P3, P3)) -> Res<SdfTexture> {
        let Some(lib) = self.texture_lib else { return f.err("the texture library did not load") };
        let unit = self.unit;
        let depth = f.len("depth")?;
        let pat = self.pattern(f, lib)?;
        let axis: u8 = match f.choice("axis", "y")? {
            "x" => 0,
            "z" => 2,
            _ => 1,
        };
        let (ts, tc) = f.opt_num("turn")?.unwrap_or(0.0).sin_cos();
        let off = f.opt_p2("offset")?;
        let [su, sv, sw] = pat.cell;
        let spacing = |gu: f64, gv: f64| match pat.feature {
            Feature::Grooves { both: true } => 0.5 / gu.hypot(gv),
            Feature::Grooves { both: false } => 0.5 / gu,
            Feature::Fixed(x) => x,
        };
        let (map, project, args, slope, feature) = match f.choice("map", "triplanar")? {
            "cylindrical" => {
                let (lo, hi) = (swizzle(axis, bounds.0), swizzle(axis, bounds.1));
                let (cx, cz) = ((lo[0] + hi[0]) * 0.5, (lo[2] + hi[2]) * 0.5);
                let r = match f.opt_len("radius")? {
                    Some(r) => r,
                    None => ((hi[0] - lo[0]) * 0.5).max((hi[2] - lo[2]) * 0.5),
                };
                // The fade: full from r1 out (the surface the depth moves stays inside it), nothing
                // inside r0: only the surface near the radius is textured, and the cells never
                // crowd towards the axis.
                let (r1, r0) = (r - 2.0 * depth.abs(), r - 4.0 * depth.abs());
                if r0 <= 0.0 {
                    return f.err("the depth is too large for the radius (at most a quarter of it)");
                }
                // The seam rule: a turn round the axis is a whole number of cells along u and v.
                let around = TAU * r;
                let nu = (around * tc / su).round();
                let nv = (-around * ts / sv).round();
                let (tu, tv) = (ts / su, tc / sv);
                let (ou, ov) = (-(nu / around * off[0] + tu * off[1]), -(nv / around * off[0] + tv * off[1]));
                // Cells per kernel unit, at r1 (the steepest the fade lets through).
                let (gu, gv) = ((nu / (TAU * r1)).hypot(tu), (nv / (TAU * r1)).hypot(tv));
                let slope = pat.slope[0] * gu + pat.slope[1] * gv + pat.slope[2] / sw + 1.0 / (r1 - r0);
                let feature = spacing((nu / around).hypot(tu), (nv / around).hypot(tv));
                let m = 1.0 / unit;
                let args = [cx * m, cz * m, nu, nv, tu * unit, tv * unit, ou, ov, r * m, sw * m, r0 * m, r1 * m];
                ("cylindrical", TexProject::Axis(axis), args.to_vec(), slope, feature)
            }
            map => {
                let (ux, uz, vx, vz) = (tc / su, ts / su, -ts / sv, tc / sv);
                let (ou, ov) = (-(ux * off[0] + uz * off[1]), -(vx * off[0] + vz * off[1]));
                let slope = pat.slope[0] / su + pat.slope[1] / sv + pat.slope[2] / sw;
                let args = [ux * unit, uz * unit, vx * unit, vz * unit, ou, ov, sw / unit];
                let project = if map == "planar" { TexProject::Axis(axis) } else { TexProject::Triplanar };
                ("planar", project, args.to_vec(), slope, spacing(1.0 / su, 1.0 / sv))
            }
        };
        let blend = f.opt_num("blend")?.unwrap_or(15f64.to_radians());
        let top = angle_mask(f.opt_num("top_angle")?.unwrap_or(0.0), blend);
        let bottom = angle_mask(f.opt_num("bottom_angle")?.unwrap_or(0.0), blend);
        let step = f.opt_len("smooth")?.unwrap_or((2.0 * depth.abs()).max(0.001 * unit));
        let mut call = vec![pat.func];
        call.extend(args.iter().map(|x| ScriptValue::from_f64(*x)));
        let kernel = self.texture_call(f, lib, map, &call)?;
        let kernel = SdfKernel::splash(self.texture_kernel(f, kernel, None)?, None);
        let mut texture = SdfTexture::new(kernel, project, 1.0 / unit, depth, step, top, bottom, slope, feature);
        texture.unit_pattern = pat.unit;
        Ok(texture)
    }

    fn pattern(&mut self, f: &Fields, lib: ScriptObject) -> Res<Pat> {
        let Some(po) = f.need("pattern")?.obj().cloned() else {
            return f.err("'pattern' is Knurl{..}, Hex{..}, Noise{..} or Pattern{..}");
        };
        let p = self.fields(&po)?;
        let num = |x: f64| ScriptValue::from_f64(x);
        Ok(match p.proto.name {
            "Knurl" => {
                let pitch = p.len("pitch")?;
                let angle = p.opt_num("angle")?.unwrap_or(30f64.to_radians());
                let flat = p.opt_num("flat")?.unwrap_or(0.2);
                if !(pitch > 0.0 && angle > 0.0 && angle < FRAC_PI_2 && (0.0..0.95).contains(&flat)) {
                    return p.err("needs a positive pitch, an angle between 0 and 90 degrees and a flat from 0 to 0.95");
                }
                let kind = p.choice("kind", "diamond")?;
                // A groove's half width w in cells, its root rounded over e = 0.4 w: the V is at most
                // g steep, 1 over its height at w (a hyperbola's slope stays under its asymptote's).
                let w = 0.5 * (1.0 - flat);
                let e = 0.4 * w;
                let g = 1.0 / ((w * w + e * e).sqrt() - e);
                let func = self.texture_call(&p, lib, &format!("knurl_{kind}"), &[num(e), num(g)])?;
                let both = kind != "straight";
                Pat { func, cell: [pitch, pitch / angle.tan(), pitch], slope: [g, if both { g } else { 0.0 }, 0.0], feature: Feature::Grooves { both }, unit: true }
            }
            "Hex" => {
                let size = p.len("size")?;
                let groove = p.opt_num("groove")?.unwrap_or(0.15);
                if !(size > 0.0 && groove > 0.0 && groove <= 0.5) {
                    return p.err("needs a positive size and a groove from 0 to 0.5");
                }
                let func = self.texture_call(&p, lib, "hex", &[num(1.0 / groove)])?;
                let s3 = 3f64.sqrt();
                Pat { func, cell: [size, size * s3, size], slope: [1.0 / groove, s3 / groove, 0.0], feature: Feature::Fixed((2.0 * groove).min(1.0 - 2.0 * groove) * size), unit: true }
            }
            "Noise" => {
                let size = p.len("size")?;
                if size <= 0.0 {
                    return p.err("needs a positive size");
                }
                // The quintic fade is at most 15/8 steep, the lattice's values 0..1 apart.
                Pat { func: self.texture_call(&p, lib, "noise", &[])?, cell: [size; 3], slope: [1.875; 3], feature: Feature::Fixed(0.5 * size), unit: true }
            }
            "Pattern" => {
                let V::Fn(func) = p.need("field")? else { return p.err("'field' is a fn(c)") };
                let func = *func;
                let cell = p.opt_p3("cell")?.unwrap_or([0.001 * self.unit; 3]);
                if cell.iter().any(|c| !(*c > 0.0)) {
                    return p.err("'cell' is three positive lengths");
                }
                let slope = match p.opt_num("slope")? {
                    Some(s) => [s.abs(); 3],
                    None => self.measured_slope(&p, func)?,
                };
                Pat { func, cell, slope, feature: Feature::Fixed(0.5 * cell[0].min(cell[1])), unit: false }
            }
            other => return f.err(format!("'pattern' is Knurl{{..}}, Hex{{..}}, Noise{{..}} or Pattern{{..}}, not {other}")),
        })
    }

    /// The library's fn `name` called with `args`: a pattern or a mapping's kernel fn.
    pub(super) fn texture_call(&mut self, f: &Fields, lib: ScriptObject, name: &str, args: &[ScriptValue]) -> Res<ScriptValue> {
        let vm = &mut *self.reader.vm;
        let func = vm.bx.heap_ref().value(lib, lut_id(name).into(), NoTrap);
        let out = vm.call(func, args);
        if out.as_object().is_none() {
            return f.err(format!("the texture library's {name} did not make a fn"));
        }
        Ok(out)
    }

    /// `m.voronoi`: the source's skin as a web of struts along the faces of cells spaced over that
    /// skin ([`super::voronoi`]), over a foam of struts along the edges of the same cells, a solid
    /// core or nothing, with solid bands at the part's lowest and highest points. The cells are the
    /// library's `vor_faces` and `vor_edges` kernels: the exact distance to the cells' faces, a
    /// continuous bound on the distance to their edges; the skin, the core and their joins are
    /// distance nodes, `round` the fillet where the struts meet.
    ///
    /// `adapt: @stress` grades the struts from `strut` to `strut_max` by the stress a solve of
    /// the source carries ([`Lower::strut_grid`]). The part always asks for that stress, and
    /// grades by what the host passes in (under `part`, the part's id): the last one solved,
    /// while the host solves the next. Until the first, the struts are `strut`.
    pub(super) fn voronoi(&mut self, f: &Fields, part: &str, of: csg_sdf::SdfNode, bounds: (P3, P3)) -> Res<csg_sdf::SdfNode> {
        use csg_sdf::SdfNode;
        use makepad_csg_graph::SdfOp;
        let Some(lib) = self.texture_lib else { return f.err("the field library did not load") };
        let unit = self.unit;
        let size = f.len("size")?;
        let strut = f.len("strut")?;
        let skin = f.opt_len("skin")?.unwrap_or(2.0 * strut);
        let random = f.opt_num("random")?.unwrap_or(0.6);
        if !(0.0..=0.7).contains(&random) {
            return f.err("'random' is 0..0.7");
        }
        let stretch = f.opt_num("stretch")?.unwrap_or(1.6);
        if !(1.0..=4.0).contains(&stretch) {
            return f.err("'stretch' is 1..4");
        }
        let axis = match f.choice("axis", "y")? {
            "x" => 0.0,
            "z" => 2.0,
            _ => 1.0,
        };
        let round = f.opt_len("round")?.unwrap_or(0.5 * strut).max(1e-4 * unit);
        let tan = match f.choice("tops", "pointed")? {
            "round" => None,
            _ => Some(f.opt_num("overhang")?.unwrap_or(45f64.to_radians()).tan()),
        };
        let salt = f.opt_num("seed")?.unwrap_or(0.0).round();
        let base = f.opt_len("base")?.unwrap_or(0.0);
        let rim = f.opt_len("rim")?.unwrap_or(0.0);
        let strut_max = f.opt_len("strut_max")?.unwrap_or(2.0 * strut);
        if strut_max < strut {
            return f.err("'strut_max' is at least 'strut'");
        }
        let graded = match f.choice("adapt", "none")? {
            "stress" => {
                self.ask_stress(part, &of, bounds, size);
                match self.solved.get(part).cloned() {
                    Some(stress) => Some(self.strut_grid(&stress, strut, strut_max, size)),
                    None => {
                        let (line, col) = f.obj.loc.unwrap_or((1, 1));
                        self.notes.push(Diag { line, col, message: format!("{part}: its struts grade by stress once its force study solves (adapt: @stress); until then they are all `strut`") });
                        None
                    }
                }
            }
            _ => None,
        };
        let mut core_kind = f.choice("core", "foam")?;
        if core_kind == "foam" {
            let depth = source_depth(&of, bounds);
            if depth <= skin {
                let (line, col) = f.obj.loc.unwrap_or((1, 1));
                let mm = 1000.0 / unit;
                self.notes.push(Diag { line, col, message: format!("{part}: skin fills this part, no foam: it is {:.1} mm deep at most and the skin reaches {:.1} mm; a smaller `skin` or `strut` leaves room for it", depth * mm, skin * mm) });
                core_kind = "none";
            }
        }
        let spec = voronoi::Spec { size, stretch, axis: axis as u8, skin, random, salt: salt as i64 as u64, base, rim, strut: if graded.is_some() { strut_max } else { strut }, thinnest: strut, round, tan, unit };
        let cells = voronoi::cells(&of, bounds, &spec, core_kind == "foam");
        let tan = tan.unwrap_or(1.0);
        let local = (bounds.0.map(|x| x / unit), bounds.1.map(|x| x / unit));
        // what a kernel reads: the struts' grid when graded, and its table of seeds
        let grid = |table: Vec<f32>| -> csg_sdf::SdfGrid {
            let samples = match &graded {
                Some((grid, _)) => grid.clone(),
                None => csg_sdf::SdfGrid::new([0.0; 3], 1.0, [2, 2, 2], 0.0, vec![0.0; 8].into()),
            };
            samples.with_table(table.into())
        };
        let mut kernel = |name: &str, args: &[f64], grid: &csg_sdf::SdfGrid| -> Res<SdfNode> {
            let call: Vec<ScriptValue> = args.iter().map(|x| ScriptValue::from_f64(*x)).collect();
            let func = self.texture_call(f, lib, name, &call)?;
            let k = SdfKernel::splash(self.texture_kernel(f, func, Some(grid))?, Some(local));
            Ok(SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale: unit, of: Box::new(SdfNode::Kernel(k)) })
        };
        let faces_grid = grid(cells.faces_table(&spec));
        let (size_k, round_k, low, high) = (size / unit, round / unit, cells.low, cells.high);
        let faces = match &graded {
            Some((_, lip)) => kernel("vor_faces_graded", &[size_k, stretch, round_k, round.min(0.5 * strut) / unit, tan, axis, low, high, *lip], &faces_grid)?,
            None => kernel("vor_faces", &[size_k, stretch, strut / unit, round_k, tan, axis, low, high], &faces_grid)?,
        };
        let core = match core_kind {
            "none" => None,
            "solid" => Some(SdfNode::Round { radius: -skin, of: Box::new(of.clone()) }),
            _ => {
                // the web's own cells: where a strut reaches the skin it lies on the web's faces, so
                // none ends in a hole of the skin and foam and web are one piece
                let edges_grid = grid(cells.edges_table(&spec));
                let edges = match &graded {
                    Some((_, lip)) => kernel("vor_edges_graded", &[size_k, stretch, axis, *lip], &edges_grid)?,
                    None => kernel("vor_edges", &[size_k, stretch, strut / unit, axis], &edges_grid)?,
                };
                // reaching half way into the skin, so the foam's struts join the web's
                let inside = SdfNode::Round { radius: -0.5 * skin, of: Box::new(of.clone()) };
                Some(SdfNode::Combine { op: SdfOp::Intersection, blend: round, of: vec![inside, edges] })
            }
        };
        // the skin: from the surface to `skin` under it
        let shell = SdfNode::Shell { thickness: 0.5 * skin, of: Box::new(SdfNode::Round { radius: -0.5 * skin, of: Box::new(of) }) };
        let web = SdfNode::Combine { op: SdfOp::Intersection, blend: round, of: vec![shell, faces] };
        Ok(match core {
            Some(core) => SdfNode::Combine { op: SdfOp::Union, blend: round, of: vec![web, core] },
            None => web,
        })
    }

    /// A part grown in the space `of` (`m.grow`). The part always asks for its shape, and is what
    /// the host passes in (under `part`, the part's id): the last one shown, within the space. Until
    /// the first, it is the space.
    pub(super) fn grow(&mut self, f: &Fields, part: &str, of: csg_sdf::SdfNode, bounds: (P3, P3)) -> Res<csg_sdf::SdfNode> {
        use csg_sdf::SdfNode;
        let Some(lib) = self.texture_lib else { return f.err("the field library did not load") };
        let unit = self.unit;
        let mass = f.opt_num("mass")?.unwrap_or(0.3);
        if !(0.05..=0.9).contains(&mass) {
            return f.err("'mass' is 0.05..0.9: the share of the solid it keeps");
        }
        let member = f.opt_len("member")?.unwrap_or(0.002 * unit);
        if !self.grow_requests.iter().any(|r| r.part == part) {
            self.grow_requests.push(super::GrowRequest { part: part.into(), lo: bounds.0, hi: bounds.1, mass, member, source: of.clone() });
        }
        let Some(shape) = self.solved.get(part).cloned() else {
            let (line, col) = f.obj.loc.unwrap_or((1, 1));
            self.notes.push(Diag { line, col, message: format!("{part}: grows on the AI hub once its force study holds it and loads it (m.fixed, m.force); until then it is its whole space") });
            // a Combine as with its shape: what the part's `of` is placed by stays the source's
            return Ok(SdfNode::Combine { op: makepad_csg_graph::SdfOp::Union, blend: 0.0, of: vec![of] });
        };
        // the kernel works in metres
        let grid = csg_sdf::SdfGrid::new(shape.origin.map(|o| (o as f64 / unit) as f32), (shape.h as f64 / unit) as f32, shape.dims, 0.0, shape.data.iter().map(|d| (*d as f64 / unit) as f32).collect());
        let func = self.texture_call(f, lib, "grown", &[])?;
        let local = (bounds.0.map(|x| x / unit), bounds.1.map(|x| x / unit));
        let k = SdfKernel::grid_read(self.texture_kernel(f, func, Some(&grid))?, local);
        let field = SdfNode::Transform { at: [0.0; 3], rotate: [0.0; 3], scale: unit, of: Box::new(SdfNode::Kernel(k)) };
        Ok(SdfNode::Combine { op: makepad_csg_graph::SdfOp::Intersection, blend: 0.0, of: vec![of, field] })
    }

    /// The struts' thickness over the source (kernel units, on the kernel's frame of the grid the
    /// host sampled `stress` on), and the Lipschitz factor that keeps the graded web a bound.
    ///
    /// A web in a skin is as strong as its relative density squared, and its density goes as its
    /// struts' thickness over the cell (Gibson and Ashby): an even safety factor wants the
    /// thickness to go as the square root of the stress. So `strut_max` where the stress reaches
    /// the reference, `strut` at least, smoothed over half a cell. The field `(face - t/2)/lip`
    /// stays a distance bound with `lip` 1 plus half the thickness's steepest slope.
    fn strut_grid(&self, stress: &csg_sdf::SdfGrid, strut: f64, strut_max: f64, size: f64) -> (csg_sdf::SdfGrid, f64) {
        let unit = self.unit;
        let n = stress.dims.map(|n| n as usize);
        let h = stress.h as f64;
        let mut t: Vec<f64> = stress.data.iter().map(|s| (strut_max * (*s as f64).max(0.0).sqrt()).clamp(strut, strut_max) / unit).collect();
        // a box filter per axis, twice (a tent), half a cell across
        let r = ((0.25 * size / h).round() as usize).max(1);
        let mut line = Vec::new();
        for _ in 0..2 {
            for a in 0..3 {
                let stride = [1, n[0], n[0] * n[1]][a];
                for start in 0..t.len() {
                    if (start / stride) % n[a] != 0 {
                        continue;
                    }
                    line.clear();
                    line.extend((0..n[a]).map(|i| t[start + i * stride]));
                    for i in 0..n[a] {
                        let (lo, hi) = (i.saturating_sub(r), (i + r).min(n[a] - 1));
                        t[start + i * stride] = line[lo..=hi].iter().sum::<f64>() / (hi - lo + 1) as f64;
                    }
                }
            }
        }
        // the trilinear read's slope along each axis is at most its steepest step along it
        let hk = h / unit;
        let mut slope = [0f64; 3];
        for (i, v) in t.iter().enumerate() {
            let c = [i % n[0], i / n[0] % n[1], i / (n[0] * n[1])];
            for a in 0..3 {
                if c[a] + 1 < n[a] {
                    slope[a] = slope[a].max((t[i + [1, n[0], n[0] * n[1]][a]] - v).abs() / hk);
                }
            }
        }
        let lip = 1.0 + 0.5 * slope.iter().map(|g| g * g).sum::<f64>().sqrt();
        let data: Vec<f32> = t.iter().map(|x| *x as f32).collect();
        (csg_sdf::SdfGrid::new(stress.origin.map(|o| (o as f64 / unit) as f32), hk as f32, stress.dims, 0.0, data.into()), lip)
    }

    /// Asks the host for the stress in a graded part's `source` over its box (`bounds`, the
    /// body's frame): about two samples a cell, at most 48 along an axis.
    fn ask_stress(&mut self, part: &str, source: &csg_sdf::SdfNode, bounds: (P3, P3), size: f64) {
        let span = (0..3).map(|a| bounds.1[a] - bounds.0[a]).fold(0.0, f64::max);
        let spacing = (0.5 * size).max(span / 47.0);
        if !self.stress_requests.iter().any(|r| r.part == part) {
            self.stress_requests.push(super::StressRequest { part: part.into(), lo: bounds.0, hi: bounds.1, spacing, source: source.clone() });
        }
    }

    pub(super) fn texture_kernel(&mut self, f: &Fields, func: ScriptValue, grid: Option<&csg_sdf::SdfGrid>) -> Res<SdfSplashExpr> {
        let Some(obj) = func.as_object() else { return f.err("a pattern is a fn(c)") };
        let compiled = match grid {
            Some(g) => SdfSplashExpr::compile_gridded(self.reader.vm, obj, SplashPoint::Vec3, &[], g.clone()),
            None => SdfSplashExpr::compile(self.reader.vm, obj, SplashPoint::Vec3, &[]),
        };
        compiled.map_err(|errors| {
            let (line, col) = f.obj.loc.unwrap_or((1, 1));
            Diag { line, col, message: format!("{}: {}", f.proto.name, errors.iter().map(|e| e.message.clone()).collect::<Vec<_>>().join("; ")) }
        })
    }

    /// A pattern's slope per cell along each axis, measured: the largest difference between
    /// neighbours on a lattice over two cells, with a margin for what falls between them.
    fn measured_slope(&mut self, f: &Fields, func: ScriptValue) -> Res<[f64; 3]> {
        const N: usize = 16;
        let k = self.texture_kernel(f, func, None)?;
        let h = 2.0 / N as f32;
        let m = N + 1;
        let mut xyz = Vec::with_capacity(m * m * m * 3);
        for z in 0..m {
            for y in 0..m {
                for x in 0..m {
                    xyz.extend([x as f32 * h, y as f32 * h, z as f32 * h]);
                }
            }
        }
        let mut t = vec![0.0f32; m * m * m];
        k.distance_batch(&xyz, &mut t);
        let at = |x: usize, y: usize, z: usize| t[(z * m + y) * m + x] as f64;
        let mut slope = [0.0f64; 3];
        for z in 0..m {
            for y in 0..m {
                for x in 0..m {
                    let v = at(x, y, z);
                    if x + 1 < m {
                        slope[0] = slope[0].max((at(x + 1, y, z) - v).abs());
                    }
                    if y + 1 < m {
                        slope[1] = slope[1].max((at(x, y + 1, z) - v).abs());
                    }
                    if z + 1 < m {
                        slope[2] = slope[2].max((at(x, y, z + 1) - v).abs());
                    }
                }
            }
        }
        Ok(slope.map(|s| 1.5 * s / h as f64))
    }
}

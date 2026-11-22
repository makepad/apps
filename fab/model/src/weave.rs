//! Weave parts (chains, chainmail, chain-art): a plate part made of separate links. Checked from the links'
//! exact geometry (gaps in and across layers, interlocks, bridges and legs) and from the sliced beads (every
//! bead given to its link; beads that reach another link fuse the pair), and run as free links in the sim.

use makepad_csg_mech::geom::P3;
use makepad_csg_mech::sim::Assembly;
use makepad_csg_mech::weave::{self, Bead, BeadCheck, Frame, SquareLink, Weave, WeaveCheck, WeaveProfile};
use makepad_csg_sdf::{LinkLoop, Links};
use std::sync::Arc;

/// PLA, g/mm^3: links print solid (a bar is a few lines wide).
pub const DENSITY: f64 = 1.24e-3;

#[derive(Clone, Debug)]
pub struct WeavePart {
    /// The plate part.
    pub part: usize,
    pub links: Arc<Links>,
    /// The links' frame (mm, Y up) to the plate (printer mm, Z up), row-major `p' = m * p`.
    pub to_plate: [[f64; 4]; 3],
    /// The view's form of it: one link mesh and where each link is.
    pub instances: Arc<LinkInstances>,
}

/// Every link is the same shape moved rigidly: one mesh (link 0, in the links' frame) and per link the
/// rigid move from link 0 onto it (in the links' frame) and its colour slot (the part's paint's).
#[derive(Clone, Debug)]
pub struct LinkInstances {
    pub mesh: makepad_csg_mesh::mesh::TriMesh,
    pub poses: Vec<[[f64; 4]; 3]>,
    pub slots: Vec<u8>,
}

/// The links a node is (under moves and uniform scales), and the map from their frame to the node's.
pub fn links_of(g: &makepad_csg_graph::Graph, id: makepad_csg_graph::NodeId) -> Option<(Arc<Links>, [[f64; 4]; 3])> {
    use makepad_csg_graph::Op;
    match &g.node(id).op {
        Op::Links { links, .. } => Some((links.clone(), [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]])),
        Op::Transform { source, at, rotate, scale } if scale[0] == scale[1] && scale[1] == scale[2] && scale[0] > 0.0 => {
            let (links, inner) = links_of(g, *source)?;
            Some((links, compose(&makepad_csg_graph::eval::transform_matrix(*at, *rotate, *scale), &inner)))
        }
        _ => None,
    }
}

/// `a` after `b`.
pub fn compose(a: &[[f64; 4]; 3], b: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    let mut out = [[0.0; 4]; 3];
    for r in 0..3 {
        for c in 0..4 {
            out[r][c] = (0..3).map(|k| a[r][k] * b[k][c]).sum::<f64>() + if c == 3 { a[r][3] } else { 0.0 };
        }
    }
    out
}

/// An orthonormal frame on the first three points of a loop that are not on one line.
fn loop_frame(pts: &[P3]) -> Option<(P3, [P3; 3])> {
    use makepad_csg_mech::geom::{cross, len, norm, sub};
    let o = pts[0];
    let x = norm(sub(*pts.get(1)?, o));
    for q in &pts[2..] {
        let z = cross(x, sub(*q, o));
        if len(z) > 1e-6 {
            let z = norm(z);
            return Some((o, [x, cross(z, x), z]));
        }
    }
    None
}

impl LinkInstances {
    pub fn new(links: &Links, sides: u32, slots: Vec<u8>) -> LinkInstances {
        use makepad_csg_mech::geom::dot;
        let first = &links.loops[0];
        let template = makepad_csg_sdf::Links::new(vec![first.clone()], links.radius, None);
        let mesh = template.mesh(sides);
        let base = loop_frame(&first.pts);
        let identity = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.0]];
        let poses = links
            .loops
            .iter()
            .map(|l| {
                let (Some((o0, f0)), Some((o1, f1))) = (base, loop_frame(&l.pts)) else { return identity };
                // R = F1 * F0^T (columns are the frame axes), t = o1 - R o0
                let mut m = [[0.0; 4]; 3];
                for r in 0..3 {
                    for c in 0..3 {
                        m[r][c] = (0..3).map(|k| f1[k][r] * f0[k][c]).sum();
                    }
                }
                for r in 0..3 {
                    m[r][3] = o1[r] - dot([m[r][0], m[r][1], m[r][2]], o0);
                }
                m
            })
            .collect();
        LinkInstances { mesh, poses, slots }
    }
}

/// The centre of a link: the mean of its centreline's points.
fn centre(l: &LinkLoop) -> P3 {
    let n = l.pts.len().max(1) as f64;
    l.pts.iter().fold([0.0; 3], |a, p| [a[0] + p[0] / n, a[1] + p[1] / n, a[2] + p[2] / n])
}

/// How far a link's colour reaches past its tube: [`crate::paint::FEATURE_BAND`] at most, and a
/// quarter of the narrowest gap between two links (the clearance the weave was built with), so
/// no link's colour reaches a neighbour's tube, even where they pass through each other.
pub fn link_band(links: &Links) -> f64 {
    let ls: Vec<weave::Link> = links.loops.iter().enumerate().map(|(i, l)| weave::Link { pts: l.pts.clone(), r: links.radius, cell: (i as i32, 0), slot: 0 }).collect();
    let reach = 4.0 * crate::paint::FEATURE_BAND;
    let gap = weave::neighbours(&ls, reach).into_iter().map(|(i, j)| weave::link_gap(&ls[i as usize], &ls[j as usize], [0.0, 1.0, 0.0]).0).fold(reach, f64::min);
    crate::paint::FEATURE_BAND.min(0.25 * gap.max(0.0))
}

/// A weave's paint by whole links: each link takes the colour `program` gives at its centre (a
/// blend of two materials one of them, dithered from link to link by its share), and the colour
/// layers become one layer per material over that material's own links, their tubes grown by
/// [`link_band`]. The support layers stay as they are. `to_model` takes the links' frame to the
/// program's (the model frame).
pub fn paint_by_link(program: &fab_paint_field::Program, links: &Links, to_model: &[[f64; 4]; 3]) -> fab_paint_field::Program {
    use fab_paint_field::{Channel, Layer, Op, Region};
    let picks: Vec<u8> = links
        .loops
        .iter()
        .enumerate()
        .map(|(k, l)| {
            let s = program.sample(Channel::Color, affine(to_model, centre(l)));
            match s.mix {
                // golden-ratio steps spread a share evenly over the links
                Some((other, share)) if share as f64 > ((k as f64 + 1.0) * 0.618_033_988_749_895).fract() => other,
                _ => s.slot,
            }
        })
        .collect();
    let scale = (0..3).map(|r| to_model[r][0] * to_model[r][0]).sum::<f64>().sqrt();
    let band = scale * link_band(links);
    let mut layers: Vec<Layer> = program.layers.iter().filter(|l| l.op.channel() != Channel::Color).cloned().collect();
    let mut slots: Vec<u8> = picks.iter().copied().filter(|&s| s != 0).collect();
    slots.sort_unstable();
    slots.dedup();
    for slot in slots {
        let own: Vec<LinkLoop> = links.loops.iter().zip(&picks).filter(|(_, p)| **p == slot).map(|(l, _)| LinkLoop { pts: l.pts.iter().map(|p| affine(to_model, *p)).collect(), slot: 0 }).collect();
        let node = makepad_csg_sdf::SdfNode::Links(Arc::new(Links::new(own, scale * links.radius, None)));
        layers.push(Layer { region: Region::Field { node, grow: band }, op: Op::Paint { channel: Channel::Color, slot } });
    }
    fab_paint_field::Program::new(layers)
}

/// Each link's colour slot in a part's paint (`to_model`: the links' frame to the paint's): the
/// paint at a point of its centreline, which [`paint_by_link`] paints with the whole link.
pub fn link_slots(program: &fab_paint_field::Program, links: &Links, to_model: &[[f64; 4]; 3]) -> Vec<u8> {
    links.loops.iter().map(|l| l.pts.first().map_or(0, |p| program.sample(fab_paint_field::Channel::Color, affine(to_model, *p)).slot)).collect()
}

pub fn affine(m: &[[f64; 4]; 3], p: P3) -> P3 {
    std::array::from_fn(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3])
}

/// The inverse of a rigid (or uniformly scaled) 3x4 map.
pub fn invert(m: &[[f64; 4]; 3]) -> [[f64; 4]; 3] {
    let a = [[m[0][0], m[0][1], m[0][2]], [m[1][0], m[1][1], m[1][2]], [m[2][0], m[2][1], m[2][2]]];
    let det = a[0][0] * (a[1][1] * a[2][2] - a[1][2] * a[2][1]) - a[0][1] * (a[1][0] * a[2][2] - a[1][2] * a[2][0]) + a[0][2] * (a[1][0] * a[2][1] - a[1][1] * a[2][0]);
    let inv = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        (a[c1][r1] * a[c2][r2] - a[c1][r2] * a[c2][r1]) / det
    };
    let mut out = [[0.0; 4]; 3];
    for r in 0..3 {
        for c in 0..3 {
            out[r][c] = inv(r, c);
        }
        out[r][3] = -(out[r][0] * m[0][3] + out[r][1] * m[1][3] + out[r][2] * m[2][3]);
    }
    out
}

impl WeavePart {
    /// `slots`: each link's colour slot ([`link_slots`]; all 0 for an unpainted part).
    pub fn new(part: usize, links: Arc<Links>, to_plate: [[f64; 4]; 3], slots: Vec<u8>) -> WeavePart {
        let instances = Arc::new(LinkInstances::new(&links, 12, slots));
        WeavePart { part, links, to_plate, instances }
    }

    pub fn up(&self) -> P3 {
        [0.0, 1.0, 0.0]
    }

    /// The links as a weave; the interlocks are the neighbour pairs that link as printed.
    pub fn weave(&self, wp: &WeaveProfile) -> Weave {
        let link = SquareLink::new(&WeaveProfile { r: self.links.radius, ..*wp }, None);
        Weave::of_links(&self.links, Frame::new([0.0; 3], self.up()), link)
    }

    /// Gaps, interlocks, bridges and legs from the exact geometry.
    pub fn check(&self, wp: &WeaveProfile) -> (WeaveCheck, (usize, usize)) {
        let w = self.weave(wp);
        (weave::check(&w, wp), w.pieces())
    }

    /// The sliced beads of this part (printer frame) judged against the links: `fused` mm is the closest a bead
    /// may come to a link it does not belong to.
    pub fn check_printed(&self, layers: &[makepad_fab_toolpath::Layer], fused: f64) -> BeadCheck {
        use makepad_fab_toolpath::Role;
        let to_model = invert(&self.to_plate);
        let mut beads = Vec::new();
        for l in layers {
            for ol in &l.objects {
                if ol.object != self.part {
                    continue;
                }
                for isl in &ol.islands {
                    for path in isl.walls.iter().chain(isl.fills.iter()) {
                        let over_air = matches!(path.role, Role::Bridge | Role::OverhangWall);
                        let z = l.print_z - 0.5 * path.height;
                        let n = path.pts.len();
                        let segs = if path.closed { n } else { n.saturating_sub(1) };
                        for k in 0..segs {
                            let (p, q) = (path.pts[k], path.pts[(k + 1) % n]);
                            let a = affine(&to_model, [p.x_mm(), p.y_mm(), z]);
                            let b = affine(&to_model, [q.x_mm(), q.y_mm(), z]);
                            beads.push(Bead { a, b, width: path.width, height: path.height, over_air });
                        }
                    }
                }
            }
        }
        weave::check_beads(&self.links, &beads, self.up(), fused)
    }

    /// Which link each island of this part's sliced layers prints, and whether every island stands on its own
    /// link's layer below (the slice judges what support a weave needs: another link under an island is air,
    /// since resting on it fuses the pair). An island may reach past its own layer below by
    /// `layer x tan(overhang_deg)` (the overhang printed without support); a piece past that is a bridge when
    /// its own link holds it on two sides (then no longer than `bridge_max`), else it may reach `3 x width`.
    pub fn check_islands(&self, layers: &[makepad_fab_toolpath::Layer], overhang_deg: f64, bridge_max: f64, width: f64) -> IslandCheck {
        use makepad_fab_slicer::clip::{difference_ex, intersection_ex, union_ex};
        use makepad_fab_slicer::geom::ExPolygon;
        use makepad_fab_slicer::offset::offset_mm;
        let to_model = invert(&self.to_plate);
        let r = self.links.radius;
        let mut out = IslandCheck::default();
        // the link nearest a printer point (model frame), with its distance to that link's surface
        let nearest = |x: f64, y: f64, z: f64| -> Option<(usize, f64)> {
            let p = affine(&to_model, [x, y, z]);
            let reach = r + 1.0;
            let mut best: Option<(usize, f64)> = None;
            self.links.segments_near(std::array::from_fn(|k| p[k] - reach), std::array::from_fn(|k| p[k] + reach), |a, b, li| {
                let d = makepad_csg_sdf::links::segment_distance(p, a, b) - r;
                if best.is_none_or(|(_, e)| d < e) {
                    best = Some((li, d));
                }
            });
            best
        };
        let mut below: std::collections::HashMap<usize, Vec<ExPolygon>> = std::collections::HashMap::new();
        let mut below_z = f64::NEG_INFINITY;
        let mut first = true;
        for l in layers {
            let slices: Vec<&ExPolygon> = l.objects.iter().filter(|o| o.object == self.part && !o.support_only).flat_map(|o| o.slices.iter()).collect();
            if slices.is_empty() {
                continue;
            }
            let z = l.print_z - 0.5 * l.height;
            let mut here: std::collections::HashMap<usize, Vec<ExPolygon>> = std::collections::HashMap::new();
            let stacked = l.print_z - below_z <= l.height + 0.01;
            for e in slices {
                out.islands += 1;
                // the island's link: the nearest link at points round its outline (two near links: it fused)
                let pts = &e.contour.pts;
                let step = (pts.len() / 24).max(1);
                let mut owners: Vec<(usize, usize)> = Vec::new();
                for p in pts.iter().step_by(step) {
                    if let Some((li, d)) = nearest(p.x_mm(), p.y_mm(), z) {
                        if d < 0.5 * width {
                            match owners.iter_mut().find(|o| o.0 == li) {
                                Some(o) => o.1 += 1,
                                None => owners.push((li, 1)),
                            }
                        }
                    }
                }
                owners.sort_by(|a, b| b.1.cmp(&a.1));
                let Some(&(own, _)) = owners.first() else { continue };
                let c = e.bbox().center();
                let at = affine(&to_model, [c.x_mm(), c.y_mm(), l.print_z]);
                if let Some(&(other, _)) = owners.get(1) {
                    out.merged.push(((own.min(other)) as u32, own.max(other) as u32, at));
                }
                here.entry(own).or_default().push(e.clone());
                if first {
                    continue;
                }
                let own_below = if stacked { below.get(&own).map(|v| v.as_slice()).unwrap_or(&[]) } else { &[] };
                let island = std::slice::from_ref(e);
                if intersection_ex(island, own_below).iter().all(|x| x.area_mm2() <= 1e-6) {
                    out.floating.push((own as u32, at));
                    continue;
                }
                let grown = offset_mm(own_below, l.height * overhang_deg.to_radians().tan() + 0.01);
                for piece in difference_ex(island, &grown) {
                    if piece.area_mm2() < (0.25 * width).powi(2) {
                        continue;
                    }
                    let anchors = intersection_ex(&offset_mm(std::slice::from_ref(&piece), 0.05), &grown);
                    let reach = farthest(&piece, &anchors);
                    let pc = piece.bbox().center();
                    let pat = affine(&to_model, [pc.x_mm(), pc.y_mm(), l.print_z]);
                    if anchors.len() >= 2 {
                        let span = 2.0 * reach;
                        if span > out.longest_bridge.0 {
                            out.longest_bridge = (span, pat);
                        }
                        if span > bridge_max {
                            out.unanchored.push((own as u32, span, pat));
                        }
                    } else if reach > 3.0 * width {
                        out.unanchored.push((own as u32, reach, pat));
                    }
                }
            }
            below = here.into_iter().map(|(k, v)| (k, union_ex(&v))).collect();
            below_z = l.print_z;
            first = false;
        }
        out.merged.sort_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        out.merged.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
        out
    }

    /// The facets the field-gradient printability check finds steeper than the overhang limit (`degrees`: its
    /// per-triangle degree for this part's printer mesh), sorted by what holds them: on the first layer (the
    /// bed), under a bar shallower than `leg` and off the bed (a bridge between the link's own legs, its span
    /// checked by [`weave::check`]), or neither: support. Areas in mm^2, and the worst unsupported facet.
    pub fn steep_facets(&self, mesh: &fab_plate::PartMesh, degrees: &[u8], leg: f64, first_layer: f64) -> SteepFacets {
        let mut out = SteepFacets::default();
        let r = self.links.radius;
        let to_model = invert(&self.to_plate);
        let up = self.up();
        for (t, &deg) in mesh.triangles.iter().zip(degrees) {
            if deg == 0 {
                continue;
            }
            let [a, b, c] = t.map(|v| mesh.positions[v as usize]);
            let n = makepad_csg_mech::geom::cross(makepad_csg_mech::geom::sub(b, a), makepad_csg_mech::geom::sub(c, a));
            let area = 0.5 * makepad_csg_mech::geom::len(n);
            if a[2].min(b[2]).min(c[2]) <= first_layer + 1e-3 {
                out.on_bed += area;
                continue;
            }
            let centre = affine(&to_model, std::array::from_fn(|k| (a[k] + b[k] + c[k]) / 3.0));
            let reach = 2.0 * r;
            let mut best: Option<(f64, P3, P3)> = None;
            self.links.segments_near(std::array::from_fn(|k| centre[k] - reach), std::array::from_fn(|k| centre[k] + reach), |p, q, _| {
                let d = makepad_csg_sdf::links::segment_distance(centre, p, q);
                if best.is_none_or(|(e, _, _)| d < e) {
                    best = Some((d, p, q));
                }
            });
            let bridge = best.is_some_and(|(_, p, q)| {
                use makepad_csg_mech::geom::{dot, len, madd, sub};
                let d = sub(q, p);
                let h = dot(d, up);
                let flat = len(madd(d, up, -h));
                let low = dot(p, up).min(dot(q, up));
                h.abs().atan2(flat) < leg - 1f64.to_radians() && low > 1.05 * r
            });
            if bridge {
                out.bridge += area;
            } else {
                out.support += area;
                if out.worst.is_none() {
                    out.worst = Some(centre);
                }
            }
        }
        out
    }

    /// The links as free bodies (no joints), ready for a scene (anchors and gravity set by the caller).
    pub fn assembly(&self, wp: &WeaveProfile) -> Assembly {
        weave::assembly(&self.weave(wp), DENSITY, makepad_csg_mech::runner::HZ)
    }
}

/// How a weave's islands stack in the slice, link by link ([`WeavePart::check_islands`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct IslandCheck {
    pub islands: usize,
    /// Islands above the first layer with nothing of their own link beneath (printed on air or on another
    /// link, which fuses): (link, where, model mm).
    pub floating: Vec<(u32, P3)>,
    /// Pieces past the overhang limit that are neither a short bridge nor a bead's reach: (link, mm, where).
    pub unanchored: Vec<(u32, f64, P3)>,
    /// The longest bridge (held on two sides by its own link), mm, and where.
    pub longest_bridge: (f64, P3),
    /// Islands that hold two links: they fused in the slice.
    pub merged: Vec<(u32, u32, P3)>,
}

impl IslandCheck {
    /// Nothing needs support and no two links share an island.
    pub fn support_free(&self) -> bool {
        self.floating.is_empty() && self.unanchored.is_empty() && self.merged.is_empty()
    }
}

/// The farthest a point of `piece`'s outline lies from the anchors' outlines, mm (outline sampled every 0.1 mm).
fn farthest(piece: &makepad_fab_slicer::geom::ExPolygon, anchors: &[makepad_fab_slicer::geom::ExPolygon]) -> f64 {
    use makepad_fab_slicer::geom::Pt;
    let edges: Vec<(Pt, Pt)> = anchors.iter().flat_map(|a| a.rings()).flat_map(|r| (0..r.pts.len()).map(move |i| (r.pts[i], r.pts[(i + 1) % r.pts.len()]))).collect();
    if edges.is_empty() {
        return f64::INFINITY;
    }
    let dist = |p: [f64; 2]| {
        edges
            .iter()
            .map(|(a, b)| {
                let (a, b) = ([a.x_mm(), a.y_mm()], [b.x_mm(), b.y_mm()]);
                let ab = [b[0] - a[0], b[1] - a[1]];
                let t = (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / (ab[0] * ab[0] + ab[1] * ab[1]).max(1e-18)).clamp(0.0, 1.0);
                ((p[0] - a[0] - t * ab[0]).powi(2) + (p[1] - a[1] - t * ab[1]).powi(2)).sqrt()
            })
            .fold(f64::INFINITY, f64::min)
    };
    let mut far = 0.0f64;
    for ring in piece.rings() {
        let n = ring.pts.len();
        for i in 0..n {
            let (a, b) = (ring.pts[i], ring.pts[(i + 1) % n]);
            let steps = (a.dist(b) / makepad_fab_slicer::geom::SCALE / 0.1).ceil().max(1.0) as usize;
            for k in 0..steps {
                let t = k as f64 / steps as f64;
                far = far.max(dist([a.x_mm() + t * (b.x_mm() - a.x_mm()), a.y_mm() + t * (b.y_mm() - a.y_mm())]));
            }
        }
    }
    far
}

/// Steep facets of a weave part by what holds them ([`WeavePart::steep_facets`]), mm^2.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SteepFacets {
    pub on_bed: f64,
    pub bridge: f64,
    /// Steep and held by nothing of its own: needs support.
    pub support: f64,
    pub worst: Option<P3>,
}

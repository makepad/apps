//! View › See-Through: the plate's parts' paint as a hologram through each part's box, each
//! material in its filament colour, the part's surface a thin shell in it, the inside denser
//! than the glow around it (`ViewportState::see`). A field part's inside is its field's; a mesh
//! part's is read from its triangles as the scene draws them, the exact distance to them and
//! inside where they wind round (`MeshField`).
//!
//! Each part's grid (`fab_paint_field::see_through`) is built on the workers, a lattice twice
//! as coarse first so it shows at once: the side of its surface once per shape, the paint over
//! the box again whenever its program changes (each state of a growing pattern). Grids go into
//! two 2D slice atlases the composite reads, the paint's colour texels and the occupancy bytes,
//! the same place in both: a brick is its nine z slices of 9 × 9 texels side by side,
//! [`PER_ROW`] bricks a row. A brick that changed is written after the last one written and
//! only the rows written since the last frame upload; a brick that did not change keeps its
//! place and costs nothing. When the atlas is full the live bricks pack from its start again,
//! in place while they fill at most half of it, else in atlases twice their size. The bricks'
//! index (one float texel each, each part's after the one before) uploads the rows that changed.
//!
//! `DrawFabSeeThrough` (`dsl.rs`) draws a part's grid box over the composite: a march from the
//! eye through it to what the G-buffer holds behind it, front to back. The parts it draws leave
//! the other passes (`AppState::is_drawn`), so the march sees through them to the plate.

use crate::api::{AppState, ElementId};
use crate::model::scene::Scene;
use crate::model::VERTEX_STRIDE;
use fab_sdf_engine::makepad_csg_sdf::mesh_field::{MeshData, MeshField};
use fab_sdf_engine::makepad_csg_sdf::SdfShape;
use fab_paint_field::see_through::{Distances, Entry, Grid, Shape, EDGE, PITCH};
use fab_paint_field::Program;
use makepad_widgets_core::makepad_platform::thread::{checkpoint, Class, TaskPool, Version};
use makepad_widgets_core::*;
use std::sync::{mpsc, Arc};

/// Bricks per atlas row.
pub const PER_ROW: usize = 100;
/// A brick's texels in the atlas: its nine slices side by side.
const TILE_W: usize = EDGE * EDGE;
const ATLAS_W: usize = PER_ROW * TILE_W;
/// The atlas's most rows of bricks (8192 texels high).
const MAX_ROWS: usize = 8192 / EDGE;
/// The index texture's width.
const INDEX_W: usize = 1024;
/// How far the hologram reaches past a part's box: this share of its largest side, at least
/// [`PAD_MM`].
const PAD: f64 = 0.06;
const PAD_MM: f64 = 3.0;

type Affine = [[f64; 4]; 3];

fn mul(a: &Affine, b: &Affine) -> Affine {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..3).map(|k| a[r][k] * b[k][c]).sum::<f64>() + if c == 3 { a[r][3] } else { 0.0 }))
}

fn apply(m: &Affine, p: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

fn det(m: &Affine) -> f64 {
    m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1]) - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0]) + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0])
}

fn invert(m: &Affine) -> Option<Affine> {
    let d = det(m);
    if d.abs() < 1e-12 {
        return None;
    }
    let a = |r: usize, c: usize| m[r][c];
    let mut out = [[0.0; 4]; 3];
    for i in 0..3 {
        for j in 0..3 {
            let (r1, r2, c1, c2) = ((j + 1) % 3, (j + 2) % 3, (i + 1) % 3, (i + 2) % 3);
            out[i][j] = (a(r1, c1) * a(r2, c2) - a(r1, c2) * a(r2, c1)) / d;
        }
    }
    for i in 0..3 {
        out[i][3] = -(0..3).map(|k| out[i][k] * m[k][3]).sum::<f64>();
    }
    Some(out)
}

fn hash(h: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(h, |h, b| (h ^ *b as u64).wrapping_mul(0x100_0000_01b3))
}

/// A finished build: what it was of and the grid, and whether it is the fine one (a coarse one
/// comes first).
struct Built {
    shape_key: u64,
    paint_key: u64,
    /// A mesh part's triangles' key (0 a field's).
    mesh_key: u64,
    shape: Arc<Shape>,
    grid: Grid,
    fine: bool,
}

struct Part {
    object: String,
    element: ElementId,
    version: Version,
    shape_key: u64,
    paint_key: u64,
    /// A mesh part's triangles' key, the fine shape's.
    mesh_key: u64,
    /// The shape at the fine lattice, once it is built.
    shape: Option<Arc<Shape>>,
    /// The grid drawn, its frame (printer mm to the grid's) and per brick its atlas slot.
    grid: Option<Grid>,
    frame: Affine,
    slots: Vec<u32>,
    job: Option<mpsc::Receiver<Built>>,
    /// The build running is of these keys.
    job_keys: (u64, u64),
    /// Its first brick's texel in the index.
    base: usize,
}

/// What a part's draw needs: its grid box's frame and its bricks.
pub struct Drawn {
    /// World (metres) from the grid's lattice (its first brick's corner at 0, a cell 1).
    pub world_from_lattice: Mat4f,
    /// The lattice from the world: three rows of an affine map.
    pub lattice_from_world: [[f32; 4]; 3],
    /// Bricks per axis, and the first one's texel in the index.
    pub grid: [f32; 4],
}

#[derive(Default)]
pub struct SeeThroughLayer {
    parts: Vec<Part>,
    /// The colour texels' atlas and the occupancies' (the same place in each).
    atlas: Option<Texture>,
    occ_atlas: Option<Texture>,
    /// Brick rows the atlases hold, and their next free slot.
    rows: usize,
    next: usize,
    index: Option<Texture>,
    index_data: Vec<f32>,
    /// Moves whenever what is drawn changes.
    revision: u64,
    /// Bytes sent to the GPU since `since` (the log's rate), the bricks that changed, and how
    /// often the atlas packed again.
    sent: u64,
    bricks_changed: u64,
    packs: u64,
    since: Option<f64>,
}

impl SeeThroughLayer {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn atlas(&self) -> Option<&Texture> {
        self.atlas.as_ref()
    }

    pub fn occ_atlas(&self) -> Option<&Texture> {
        self.occ_atlas.as_ref()
    }

    pub fn index(&self) -> Option<&Texture> {
        self.index.as_ref()
    }

    /// The atlas's and the index's sizes (texels).
    pub fn sizes(&self) -> [f32; 4] {
        [ATLAS_W as f32, (self.rows * EDGE).max(1) as f32, INDEX_W as f32, (self.index_data.len() / 4 / INDEX_W).max(1) as f32]
    }

    /// The parts drawn see-through now (their grids are on the GPU).
    pub fn drawn_elements(&self) -> Vec<ElementId> {
        self.parts.iter().filter(|p| p.grid.is_some()).map(|p| p.element).collect()
    }

    /// Each drawn part's box.
    pub fn drawn(&self) -> Vec<Drawn> {
        self.parts
            .iter()
            .filter_map(|p| {
                let g = p.grid.as_ref()?;
                let printer_from_grid = invert(&p.frame)?;
                // lattice l: grid mm (l + 8 min) cell, printer mm, world metres
                let grid_from_lattice: Affine = std::array::from_fn(|r| std::array::from_fn(|c| if c == 3 { g.min[r] as f64 * 8.0 * g.cell } else if c == r { g.cell } else { 0.0 }));
                let mut world = mul(&printer_from_grid, &grid_from_lattice);
                for row in &mut world {
                    for v in row.iter_mut() {
                        *v *= 0.001;
                    }
                }
                let back = invert(&world)?;
                let mut m = Mat4f::identity();
                for r in 0..3 {
                    for c in 0..4 {
                        m.v[c * 4 + r] = world[r][c] as f32;
                    }
                }
                Some(Drawn {
                    world_from_lattice: m,
                    lattice_from_world: back.map(|r| r.map(|v| v as f32)),
                    grid: [g.dim[0] as f32, g.dim[1] as f32, g.dim[2] as f32, p.base as f32],
                })
            })
            .collect()
    }

    /// The plate's parts in step with `state`: builds started, finished ones written into
    /// the atlas. Off, it lets everything go. True when what it draws changed.
    pub fn sync(&mut self, cx: &mut Cx, state: &AppState, paint: &super::paint::PaintLayer) -> bool {
        if !state.view.see.on {
            if self.parts.is_empty() {
                return false;
            }
            self.parts.clear();
            self.revision += 1;
            return true;
        }
        let before = self.parts.len();
        let poses = state.shared_poses();
        // a part shows by its field, or by its triangles in the scene (a mesh part)
        let solid = |p: &crate::api::PlatePart| p.sdf.is_some() || p.element.and_then(|e| state.scene.element(e)).is_some_and(|e| e.has_geometry());
        self.parts.retain(|v| state.plate.parts.iter().any(|p| p.object == v.object && p.element == Some(v.element) && solid(p) && state.see_through_shows_under(poses, v.element)));
        let mut changed = self.parts.len() != before;
        for p in &state.plate.parts {
            let Some(element) = p.element else { continue };
            if !solid(p) || !state.see_through_shows_under(poses, element) {
                continue;
            }
            let baked = paint.baked(&p.object);
            // the grid's frame: the paint's, else the shape's own (a field's) or the part's model
            // frame (a mesh's); the shape's key: the field's and where it sits, or the scene's
            // build of the part's triangles (the build reads whether they changed)
            let (frame, mut shape_key, source) = match &p.sdf {
                Some(sdf) => {
                    let Some(shape_frame) = invert(&sdf.to_printer) else { continue };
                    let frame = baked.map_or(shape_frame, |(_, _, from)| from);
                    let grid_from_shape = mul(&frame, &sdf.to_printer);
                    (frame, hash(0xcbf2_9ce4_8422_2325, &sdf.key), Source::Field(sdf.shape.clone(), grid_from_shape))
                }
                None => {
                    let frame = baked.map_or(p.model_from_printer, |(_, _, from)| from);
                    let key = hash(hash(0x8422_2325_cbf2_9ce4, &state.scene.generation.to_le_bytes()), &element.0.to_le_bytes());
                    (frame, key, Source::Mesh(state.scene.clone(), element))
                }
            };
            for v in frame.iter().flatten() {
                shape_key = hash(shape_key, &v.to_bits().to_le_bytes());
            }
            let own = [p.color[0], p.color[1], p.color[2]];
            let mut paint_key = hash(shape_key, &baked.map_or(0, |(program, _, _)| Arc::as_ptr(program) as usize).to_le_bytes());
            for v in baked.map_or(&[][..], |(_, palette, _)| palette).iter().flatten().chain(&own) {
                paint_key = hash(paint_key, &v.to_bits().to_le_bytes());
            }
            let i = match self.parts.iter().position(|v| v.object == p.object) {
                Some(i) => i,
                None => {
                    self.parts.push(Part { object: p.object.clone(), element, version: Version::new(), shape_key: 0, paint_key: 0, mesh_key: 0, shape: None, grid: None, frame, slots: Vec::new(), job: None, job_keys: (0, 0), base: 0 });
                    self.parts.len() - 1
                }
            };
            let v = &mut self.parts[i];
            v.element = element;
            if (v.shape_key, v.paint_key) == (shape_key, paint_key) || (v.job.is_some() && v.job_keys == (shape_key, paint_key)) {
                continue;
            }
            // a newer paint waits for the build of the one before: each state shows
            if v.job.is_some() && v.job_keys.0 == shape_key {
                continue;
            }
            let shape = v.shape.clone().filter(|_| v.shape_key == shape_key);
            // a mesh part's shape from before and its triangles' key: kept when they are the same
            let before = v.shape.clone().map(|s| (v.mesh_key, s));
            let paint = baked.map(|(program, palette, _)| (program.clone(), palette.to_vec()));
            let (tx, rx) = mpsc::channel();
            let pool = cx.task_pool();
            v.frame = frame;
            v.job_keys = (shape_key, paint_key);
            let _ = cx.task_pool().submit_at(Class::Visible, "fab see-through grid", v.version.next(), move || {
                let fan = |len: usize, f: &(dyn Fn(usize) + Sync)| job_fan(&pool, len, f);
                let (program, palette): (Option<&Program>, &[[f32; 3]]) = match &paint {
                    Some((program, palette)) => (Some(&**program), palette),
                    None => (None, &[]),
                };
                let colour = |shape: &Shape| {
                    let t = std::time::Instant::now();
                    let grid = Grid::colour(shape, program, palette, own, &fan)?;
                    log!("fab vp see-through: coloured {} bricks at {} mm, {} tiles, {:.0} ms", shape.bricks(), shape.cell, grid.tiles(), t.elapsed().as_secs_f64() * 1000.0);
                    Some(grid)
                };
                let recolour = |shape: Arc<Shape>, mesh_key: u64| {
                    if let Some(grid) = colour(&shape) {
                        let _ = tx.send(Built { shape_key, paint_key, mesh_key, shape, grid, fine: true });
                    }
                };
                if let Some(shape) = shape {
                    return recolour(shape, before.map_or(0, |(k, _)| k));
                }
                // the signed distance in the grid's frame (mm), how far its values prove its surface away, and the box
                let (distance, clearance, (min, max), mesh_key): (Box<Distances>, fab_sdf_engine::makepad_csg_sdf::Clearance, ([f64; 3], [f64; 3]), u64) = match source {
                    Source::Field(field, grid_from_shape) => {
                        let Some(shape_from_grid) = invert(&grid_from_shape) else { return };
                        let scale = det(&grid_from_shape).abs().cbrt();
                        let sdf = field.field();
                        let (lo, hi) = (field.min, field.max);
                        let (mut min, mut max) = ([f64::MAX; 3], [f64::MIN; 3]);
                        for c in 0..8 {
                            let q = apply(&grid_from_shape, [if c & 1 == 0 { lo[0] } else { hi[0] }, if c & 2 == 0 { lo[1] } else { hi[1] }, if c & 4 == 0 { lo[2] } else { hi[2] }]);
                            for k in 0..3 {
                                min[k] = min[k].min(q[k]);
                                max[k] = max[k].max(q[k]);
                            }
                        }
                        let distance = move |qs: &[[f64; 3]], out: &mut [f64]| {
                            for (q, d) in qs.iter().zip(out) {
                                let s = apply(&shape_from_grid, *q);
                                *d = scale * fab_sdf_engine::makepad_csg_sdf::Sdf3::distance(&*sdf, makepad_csg_math::Vec3d::new(s[0], s[1], s[2]));
                            }
                        };
                        (Box::new(distance), field.clearance().scaled(scale), (min, max), 0)
                    }
                    Source::Mesh(scene, element) => {
                        // the exact distance to its triangles, inside where they wind round
                        let t = std::time::Instant::now();
                        let mesh = MeshField::new(mesh_of(&scene, element, &frame));
                        drop(scene);
                        let key = mesh.key();
                        if let Some((_, shape)) = before.filter(|(k, _)| *k == key) {
                            return recolour(shape, key);
                        }
                        if mesh.data.triangles.is_empty() {
                            return;
                        }
                        log!("fab vp see-through: mesh of {} triangles ready in {:.0} ms", mesh.data.triangles.len(), t.elapsed().as_secs_f64() * 1000.0);
                        let bounds = (mesh.min.map(|v| v as f64), mesh.max.map(|v| v as f64));
                        let distance = move |qs: &[[f64; 3]], out: &mut [f64]| mesh_distances(&mesh, qs, out);
                        (Box::new(distance), fab_sdf_engine::makepad_csg_sdf::Clearance::uniform(1.0), bounds, key)
                    }
                };
                let pad = (0..3).map(|k| max[k] - min[k]).fold(0.0, f64::max) * PAD;
                let pad = pad.max(PAD_MM);
                // twice as coarse first, so the part shows at once; then the lattice it keeps
                let cell = Shape::cell_for((min, max), pad, PITCH);
                for (cell, fine) in [(2.0 * cell, false), (cell, true)] {
                    let t = std::time::Instant::now();
                    let Some(shape) = Shape::bake(&*distance, clearance, (min, max), pad, cell, &fan) else { return };
                    log!("fab vp see-through: shape at {} mm, {} skin bricks of {}, {:.0} ms", shape.cell, shape.edges(), shape.bricks(), t.elapsed().as_secs_f64() * 1000.0);
                    let shape = Arc::new(shape);
                    let Some(grid) = colour(&shape) else { return };
                    if tx.send(Built { shape_key, paint_key, mesh_key, shape, grid, fine }).is_err() {
                        return;
                    }
                }
            });
            v.job = Some(rx);
        }
        // the builds that finished, written
        let mut built = Vec::new();
        for (i, v) in self.parts.iter_mut().enumerate() {
            // the newest the build sent (the coarse one, the fine one or both)
            let mut newest = None;
            while let Some(rx) = &v.job {
                match rx.try_recv() {
                    Ok(b) => {
                        if b.fine {
                            v.job = None;
                        }
                        newest = Some(b);
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => v.job = None,
                }
            }
            if let Some(b) = newest {
                built.push((i, b));
            }
        }
        if !built.is_empty() {
            self.install(cx, built);
            changed = true;
        }
        if changed {
            self.write_index(cx);
            self.revision += 1;
        }
        changed
    }

    /// Finished grids into the atlas: each brick whose texels changed at the next free slot.
    fn install(&mut self, cx: &mut Cx, built: Vec<(usize, Built)>) {
        // which bricks are new: per part, per brick, the tile to write (or the slot it keeps)
        let mut writes = 0usize;
        let mut plans: Vec<(usize, Built, Vec<Option<u32>>)> = Vec::new();
        for (i, b) in built {
            let v = &self.parts[i];
            let same_lattice = v.grid.as_ref().is_some_and(|g| g.dim == b.grid.dim && g.min == b.grid.min && g.cell == b.grid.cell);
            let keep: Vec<Option<u32>> = b
                .grid
                .entries
                .iter()
                .enumerate()
                .map(|(k, e)| match (e, v.grid.as_ref().filter(|_| same_lattice).map(|g| (g, g.entries[k]))) {
                    (Entry::Tile(t), Some((g, Entry::Tile(o)))) if g.tile(o) == b.grid.tile(*t) && g.tile_occ(o) == b.grid.tile_occ(*t) => Some(v.slots[k]),
                    _ => None,
                })
                .collect();
            writes += b.grid.entries.iter().zip(&keep).filter(|(e, k)| matches!(e, Entry::Tile(_)) && k.is_none()).count();
            plans.push((i, b, keep));
        }
        let capacity = self.rows * PER_ROW;
        // the rows to send: from the first written to the last
        let mut first = self.next;
        let mut data = match (&self.atlas, &self.occ_atlas) {
            (Some(t), Some(o)) if self.next + writes <= capacity => (t.take_vec_u32(cx), o.take_vec_u8(cx)),
            atlases => {
                // full: the live bricks pack from the start again, in place while they take at
                // most half of it (so the next fill is as far away), else in atlases twice
                // their size
                let live: usize = self.parts.iter().enumerate().filter(|(i, _)| !plans.iter().any(|(j, _, _)| j == i)).map(|(_, v)| v.grid.as_ref().map_or(0, |g| g.tiles())).sum::<usize>() + plans.iter().map(|(_, b, _)| b.grid.tiles()).sum::<usize>();
                let rows = (2 * live).max(1024).div_ceil(PER_ROW).min(MAX_ROWS);
                let mut data = match atlases {
                    (Some(t), Some(o)) if rows <= self.rows => (t.take_vec_u32(cx), o.take_vec_u8(cx)),
                    _ => {
                        self.rows = rows;
                        self.atlas = None;
                        self.occ_atlas = None;
                        (vec![0u32; ATLAS_W * self.rows * EDGE], vec![0u8; ATLAS_W * self.rows * EDGE])
                    }
                };
                self.next = 0;
                self.packs += 1;
                first = 0;
                for (_, _, keep) in &mut plans {
                    keep.iter_mut().for_each(|k| *k = None);
                }
                for i in 0..self.parts.len() {
                    if plans.iter().any(|(j, _, _)| *j == i) {
                        continue;
                    }
                    if let Some(g) = self.parts[i].grid.take() {
                        let slots = self.write_tiles(&mut data, &g, &vec![None; g.entries.len()]);
                        self.parts[i].slots = slots;
                        self.parts[i].grid = Some(g);
                    }
                }
                data
            }
        };
        self.bricks_changed += writes as u64;
        for (i, b, keep) in plans {
            let slots = self.write_tiles(&mut data, &b.grid, &keep);
            let v = &mut self.parts[i];
            v.slots = slots;
            v.grid = Some(b.grid);
            v.shape = b.fine.then_some(b.shape);
            v.mesh_key = b.mesh_key;
            v.shape_key = b.shape_key;
            v.paint_key = b.paint_key;
        }
        let (colours, occ) = data;
        match (&self.atlas, &self.occ_atlas) {
            (Some(t), Some(o)) => {
                let (r0, r1) = ((first / PER_ROW) * EDGE, self.next.div_ceil(PER_ROW) * EDGE);
                let rect = if r1 > r0 { RectUsize::new(PointUsize::new(0, r0), SizeUsize::new(ATLAS_W, r1 - r0)) } else { RectUsize::new(PointUsize::new(0, 0), SizeUsize::new(0, 0)) };
                self.sent += (rect.size.width * rect.size.height * 5) as u64;
                t.put_back_vec_u32(cx, colours, Some(rect));
                o.put_back_vec_u8(cx, occ, Some(rect));
            }
            _ => {
                self.sent += (colours.len() * 5) as u64;
                let (width, height) = (ATLAS_W, self.rows * EDGE);
                self.atlas = Some(Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 { width, height, data: Some(colours), updated: TextureUpdated::Full }));
                self.occ_atlas = Some(Texture::new_with_format(cx, TextureFormat::VecRu8 { width, height, data: Some(occ), unpack_row_length: None, updated: TextureUpdated::Full }));
            }
        }
        self.log_rate();
    }

    /// `grid`'s tiles into the atlases: a kept one at its slot, the rest at the next free ones.
    /// Per brick its slot (`u32::MAX`: no tile).
    fn write_tiles(&mut self, data: &mut (Vec<u32>, Vec<u8>), grid: &Grid, keep: &[Option<u32>]) -> Vec<u32> {
        let mut slots = vec![u32::MAX; grid.entries.len()];
        for (k, e) in grid.entries.iter().enumerate() {
            let Entry::Tile(t) = e else { continue };
            if let Some(s) = keep[k] {
                slots[k] = s;
                continue;
            }
            // past the most the atlas holds the brick shows as air
            if self.next >= self.rows * PER_ROW {
                continue;
            }
            let s = self.next;
            self.next += 1;
            let (tx, ty) = (s % PER_ROW, s / PER_ROW);
            let (tile, occ) = (grid.tile(*t), grid.tile_occ(*t));
            for z in 0..EDGE {
                for y in 0..EDGE {
                    let row = (ty * EDGE + y) * ATLAS_W + tx * TILE_W + z * EDGE;
                    let from = (z * EDGE + y) * EDGE;
                    data.0[row..row + EDGE].copy_from_slice(&tile[from..from + EDGE]);
                    data.1[row..row + EDGE].copy_from_slice(&occ[from..from + EDGE]);
                }
            }
            slots[k] = s as u32;
        }
        slots
    }

    /// Every part's bricks in the index: air -1, solid -2 and its colour, a glow -3 and its
    /// colour, else its atlas slot.
    fn write_index(&mut self, cx: &mut Cx) {
        let mut data = Vec::new();
        for v in &mut self.parts {
            v.base = data.len() / 4;
            let Some(g) = &v.grid else { continue };
            for (k, e) in g.entries.iter().enumerate() {
                match e {
                    Entry::Air => data.extend([-1.0, 0.0, 0.0, 0.0]),
                    Entry::Solid(t) | Entry::Glow(t) => {
                        let (rgb, _) = fab_paint_field::see_through::untexel(*t);
                        data.extend([if matches!(e, Entry::Solid(_)) { -2.0 } else { -3.0 }, rgb[0], rgb[1], rgb[2]]);
                    }
                    Entry::Tile(_) if v.slots[k] == u32::MAX => data.extend([-1.0, 0.0, 0.0, 0.0]),
                    Entry::Tile(_) => data.extend([v.slots[k] as f32, 0.0, 0.0, 0.0]),
                }
            }
        }
        let rows = (data.len() / 4).div_ceil(INDEX_W).max(1);
        data.resize(rows * INDEX_W * 4, 0.0);
        let same_size = self.index_data.len() == data.len() && self.index.is_some();
        if same_size {
            let row = INDEX_W * 4;
            let differ: Vec<usize> = (0..rows).filter(|r| data[r * row..(r + 1) * row] != self.index_data[r * row..(r + 1) * row]).collect();
            if let (Some(&r0), Some(&r1)) = (differ.first(), differ.last()) {
                let t = self.index.as_ref().unwrap();
                let _ = t.take_vec_f32(cx);
                self.sent += ((r1 + 1 - r0) * row * 4) as u64;
                t.put_back_vec_f32(cx, data.clone(), Some(RectUsize::new(PointUsize::new(0, r0), SizeUsize::new(INDEX_W, r1 + 1 - r0))));
            }
        } else {
            self.sent += (data.len() * 4) as u64;
            self.index = Some(Texture::new_with_format(cx, TextureFormat::VecRGBAf32 { width: INDEX_W, height: rows, data: Some(data.clone()), updated: TextureUpdated::Full }));
        }
        self.index_data = data;
    }

    /// Once a second while it sends: how fast.
    fn log_rate(&mut self) {
        let now = Cx::time_now();
        let since = *self.since.get_or_insert(now);
        if now - since >= 1.0 {
            let rate = |v: u64| v as f64 / (now - since);
            log!("fab vp see-through: sent {:.2} MB/s over {:.1} s ({:.0} changed bricks a second, {:.2} MB/s of tiles; {} packs); atlas {} of {} bricks", rate(self.sent) / 1e6, now - since, rate(self.bricks_changed), rate(self.bricks_changed * (EDGE * EDGE * EDGE * 4) as u64) / 1e6, self.packs, self.next, self.rows * PER_ROW);
            self.sent = 0;
            self.bricks_changed = 0;
            self.packs = 0;
            self.since = Some(now);
        }
    }
}

/// A build's bricks on the pool's workers: a newer build of the part ends this one.
/// What a part's inside is read from: its distance field (and the grid's frame from the
/// field's), or its triangles as the scene draws them.
enum Source {
    Field(Arc<SdfShape>, Affine),
    Mesh(Arc<Scene>, ElementId),
}

/// How far two points' empty balls must overlap for one to take the other's side (mm): more than
/// the distances' rounding.
const SHARED_MM: f64 = 1e-3;

/// The signed distance to `mesh` at `qs`, into `out`: the distance to its nearest triangle, and
/// inside where its winding number is over a half. Two points whose empty balls overlap have no
/// surface between them, so a point takes the side of an earlier one it overlaps by more than
/// [`SHARED_MM`] (a point on the surface has either side); only the rest count the winding.
fn mesh_distances(mesh: &MeshField, qs: &[[f64; 3]], out: &mut [f64]) {
    for i in 0..qs.len() {
        let q = qs[i];
        let p = q.map(|v| v as f32);
        let d = mesh.unsigned(p) as f64;
        let shared = (0..i).rev().find(|&j| {
            let o = qs[j];
            ((q[0] - o[0]).powi(2) + (q[1] - o[1]).powi(2) + (q[2] - o[2]).powi(2)).sqrt() + SHARED_MM < d + out[j].abs()
        });
        let inside = match shared {
            Some(j) => out[j] < 0.0,
            None => mesh.winding(p) > 0.5,
        };
        out[i] = if inside { -d } else { d };
    }
}

/// Element `element`'s triangles in `scene` (world metres) in the grid's frame (`grid_from_printer`,
/// printer mm), each vertex once.
fn mesh_of(scene: &Scene, element: ElementId, grid_from_printer: &Affine) -> MeshData {
    let mut data = MeshData::default();
    let Some(e) = scene.element(element) else { return data };
    for &(batch, first, count) in &e.ranges {
        let Some(b) = scene.batches.get(batch as usize) else { continue };
        let Some(indices) = b.indices.get(first as usize..(first + count) as usize) else { continue };
        let mut at = std::collections::HashMap::new();
        for t in indices.chunks_exact(3) {
            let tri: [u32; 3] = std::array::from_fn(|k| {
                *at.entry(t[k]).or_insert_with(|| {
                    let v = &b.vertices[t[k] as usize * VERTEX_STRIDE..];
                    let p = apply(grid_from_printer, [v[0] as f64 * 1000.0, v[1] as f64 * 1000.0, v[2] as f64 * 1000.0]);
                    data.positions.push(p.map(|x| x as f32));
                    (data.positions.len() - 1) as u32
                })
            });
            data.triangles.push(tri);
        }
    }
    data
}

fn job_fan(pool: &TaskPool, len: usize, f: &(dyn Fn(usize) + Sync)) {
    pool.fan_out(len, |i| {
        if checkpoint().is_ok() {
            f(i)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A box from `lo` to `hi`, its triangles wound outward.
    fn cube(lo: f32, hi: f32) -> MeshField {
        let positions = (0..8).map(|i| [if i & 1 == 0 { lo } else { hi }, if i & 2 == 0 { lo } else { hi }, if i & 4 == 0 { lo } else { hi }]).collect();
        let quads = [[0, 2, 3, 1], [4, 5, 7, 6], [0, 1, 5, 4], [2, 6, 7, 3], [0, 4, 6, 2], [1, 3, 7, 5]];
        let triangles = quads.iter().flat_map(|q: &[u32; 4]| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]]).collect();
        MeshField::new(MeshData { positions, triangles })
    }

    #[test]
    fn a_mesh_reads_the_side_each_point_is_on_even_beside_one_on_its_surface() {
        let mesh = cube(-5.0, 5.0);
        // a brick's reads across the box's face, x fastest
        let n = 5;
        let qs: Vec<[f64; 3]> = (0..n * n * n).map(|r| [3.0 + (r % n) as f64 * 1.0, -1.0 + ((r / n) % n) as f64 * 0.5, -1.0 + (r / (n * n)) as f64 * 0.5]).collect();
        let mut out = vec![0.0; qs.len()];
        mesh_distances(&mesh, &qs, &mut out);
        for (q, d) in qs.iter().zip(&out) {
            let want = mesh.signed(q.map(|v| v as f32)) as f64;
            assert!((d - want).abs() < 1e-5, "at {q:?}: {d} against {want}");
        }
        assert!(out.iter().any(|d| *d < 0.0) && out.iter().any(|d| *d > 0.0));
    }
}

//! `Scene` — the immutable, shareable product of a load. Built once from a
//! [`ModelData`] on the loader thread; consumed by every downstream layer via
//! `Arc<Scene>`.
//!
//! Build steps (all inside [`Scene::from_model`]):
//! 1. normalise units / up-axis / handedness (see `model.rs` conventions)
//! 2. elements
//! 3. plan the batches: one [`RenderBatch`] per **(material × spatial cell)**,
//!    sized so a whole batch is a meaningful unit of frustum culling — the
//!    Metal backend draws a geometry's whole index buffer, so a batch is the
//!    smallest thing that can be culled at all
//! 4. merge geometry into those batches in world space, stamping
//!    `Vertex::element`, de-duplicating each placement's vertices
//! 5. per-element bounds, coplanar overlap resolution, scene bounds
//! 6. spatial index ([`Bvh`])

use crate::model::batch::{ElementRange, RenderBatch, Vertex, VERTEX_STRIDE};
use crate::model::bounds::{
    aabb_center, aabb_empty, aabb_is_empty, aabb_union, aabb_union_point, transform_dir,
    transform_point,
};
use crate::model::bvh::{Bvh, PickOptions};
use crate::model::ids::{ElementId, MaterialId, MeshId};
use crate::model::model::{Handedness, MaterialData, ModelData, UpAxis};
use crate::model::overlap::{self, OverlapReport, DEFAULT_COPLANAR_TOL_M};
use crate::model::query::{Frustum, Ray, RayHit};
use crate::model::state::SceneState;
use crate::model::units::Units;
use makepad_math::{Aabb, Mat4f, Vec3f};
use makepad_widgets_core::makepad_platform::thread::TaskPool;
use std::collections::HashMap;
use std::sync::Arc;

/// Triangles a spatial batch aims for. Big enough that draw-call count stays
/// low (5 M triangles ≈ 40–60 batches), small enough that a batch outside the
/// frustum is worth skipping.
pub const TARGET_BATCH_TRIANGLES: usize = 128 * 1024;
/// Hard ceiling on vertices per batch, so one pathological material can never
/// produce a single multi-hundred-megabyte upload.
pub const MAX_BATCH_VERTICES: usize = 1 << 20;

#[derive(Clone, Debug)]
pub struct Element {
    pub id: ElementId,
    pub name: String,
    /// World-space bounds of this element's own triangles (a part drawn from its distance
    /// field, which has none: its field's box, [`Scene::set_field_bounds`]). Empty
    /// (inverted) for an element without geometry.
    pub bounds: Aabb,
    /// Where this element's triangles live: `(batch index, first_index, index_count)`.
    pub ranges: Vec<(u32, u32, u32)>,
    pub triangle_count: u32,
}

impl Element {
    pub fn has_geometry(&self) -> bool {
        self.triangle_count > 0
    }
}

/// Resolved material (== `MaterialData` with the texture moved into the
/// scene's texture list).
#[derive(Clone, Debug)]
pub struct Material {
    pub id: MaterialId,
    pub name: String,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
    pub ior: f32,
    pub transmission: f32,
    pub double_sided: bool,
    pub transparent: bool,
    /// Index into `Scene::textures`, if textured.
    pub texture: Option<u32>,
}

/// One source mesh placement/material part and its render-only coplanar
/// conflict metadata. Canonical geometry remains in [`RenderBatch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScenePart {
    pub id: u32,
    pub element: ElementId,
    pub element_part: u32,
    pub mesh: MeshId,
    pub material: MaterialId,
    pub batch: u32,
    pub first_index: u32,
    pub index_count: u32,
    /// Higher wins only inside `coplanar_group`; zero means no conflict.
    pub draw_priority: u16,
    pub coplanar_group: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SceneStats {
    pub elements: u32,
    pub elements_with_geometry: u32,
    pub triangles: u32,
    pub vertices: u32,
    pub batches: u32,
    pub materials: u32,
    pub textures: u32,
    pub bvh_nodes: u32,
    pub build_ms: f32,
    /// Bytes of CPU geometry held by `batches`.
    pub geometry_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct Scene {
    pub name: String,
    pub source_path: Option<std::path::PathBuf>,
    pub units: Units,
    pub elements: Vec<Element>,
    pub materials: Vec<Material>,
    pub batches: Vec<RenderBatch>,
    pub parts: Vec<ScenePart>,
    pub overlap_report: OverlapReport,
    pub bounds: Aabb,
    pub bvh: Bvh,
    pub stats: SceneStats,
    /// Monotonic per process; the renderer re-uploads when it changes.
    pub generation: u64,
    /// Decoded textures, moved out of `MaterialData` (index == `Material::texture`).
    pub textures: Vec<crate::model::model::TextureData>,
    /// The scene packed into the view's lit model and prepared for upload (decoded, mipped,
    /// validated) on the thread that built the scene, so the UI thread only uploads it: a
    /// committed plate never stalls a frame (nor the headset's) on packing it.
    pub prepared: Option<Arc<PreparedModel>>,
}

/// [`Scene::prepared`]: the upload-ready lit model of a scene.
pub struct PreparedModel(pub makepad_render::PreparedStaticPreview);

impl std::fmt::Debug for PreparedModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PreparedModel")
    }
}

static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// One contiguous run of triangles: an element's slice of one mesh placement
/// under one material. The unit the batch planner shuffles.
struct Placement {
    element: u32,
    part: u32,
    element_part: u32,
    /// Index into `model.meshes`.
    mesh: u32,
    world: Mat4f,
    material: MaterialId,
    first_index: u32,
    index_count: u32,
    triangles: u32,
    center: Vec3f,
    /// Filled by the planner.
    batch: u32,
}

impl Scene {
    /// Build the optimized runtime scene from the canonical editable document.
    pub fn from_document(
        document: crate::document::Document,
        progress: &mut dyn FnMut(f32),
    ) -> Scene {
        Scene::from_model(document.into_model_data(), progress)
    }

    /// As [`Scene::from_document`], with named build stages for progress UI.
    pub fn from_document_with(
        document: crate::document::Document,
        progress: &mut dyn FnMut(&'static str, f32),
    ) -> Scene {
        Scene::from_model_with(document.into_model_data(), progress)
    }

    /// A scene with nothing in it. Bounds are empty; `generation` is 0 so the
    /// renderer treats it as "nothing to upload".
    pub fn empty() -> Scene {
        Scene {
            name: String::new(),
            source_path: None,
            units: Units::default(),
            elements: Vec::new(),
            materials: Vec::new(),
            batches: Vec::new(),
            parts: Vec::new(),
            overlap_report: OverlapReport::default(),
            bounds: aabb_empty(),
            bvh: Bvh::default(),
            stats: SceneStats::default(),
            generation: 0,
            textures: Vec::new(),
            prepared: None,
        }
    }

    /// Build using an existing scene's overlap report when all analysis inputs still match.
    /// The caller owns that scene; no geometry or report is retained globally.
    /// `pool`: the workers the build's big passes spread over (the overlap analysis, the pointer's
    /// tree, the lit model's packing): the same scene as without.
    pub fn from_document_reusing(document: crate::document::Document, previous: Option<&Scene>, pool: Option<&TaskPool>, progress: &mut dyn FnMut(&'static str, f32)) -> Scene {
        Self::from_model_reusing(document.into_model_data(), previous, &[], pool, progress)
    }

    /// Build with `copies`: elements that repeat another element's triangles, `(copy,
    /// original)` by name, and are drawn in a pass of their own, never in a depth buffer with
    /// the rest (a joint check's ghosts: each joint's moving part again, once per way it turns).
    /// The coplanar overlap pass leaves them out: where a copy rests, each of its triangles lies
    /// on its original's, and pairing them all is gigabytes and most of the build. Each part of
    /// a copy takes the draw priority and group its original's parts got (the highest), so
    /// copies meet one another as their originals do.
    pub fn from_document_with_copies(document: crate::document::Document, copies: &[(String, String)], progress: &mut dyn FnMut(&'static str, f32)) -> Scene {
        Self::from_model_reusing(document.into_model_data(), None, copies, None, progress)
    }

    /// Build the scene. Runs on the loader thread; never call from the UI
    /// thread with a large model. `progress` gets `0..=1` for the build stage.
    pub fn from_model(model: ModelData, progress: &mut dyn FnMut(f32)) -> Scene {
        Scene::from_model_with(model, &mut |_, f| progress(f))
    }

    /// As [`Scene::from_model`], with a named stage alongside the fraction so
    /// the loader can say what it is doing ("batches", "overlap", "bvh").
    pub fn from_model_with(model: ModelData, progress: &mut dyn FnMut(&'static str, f32)) -> Scene {
        Self::from_model_reusing(model, None, &[], None, progress)
    }

    fn from_model_reusing(mut model: ModelData, previous: Option<&Scene>, copies: &[(String, String)], pool: Option<&TaskPool>, progress: &mut dyn FnMut(&'static str, f32)) -> Scene {
        let t0 = std::time::Instant::now();
        let scale = model.units.source_to_meters;
        let normalise = normalisation_matrix(model.up_axis, scale);
        let flip_winding = model.handedness == Handedness::Left;

        // ---- 1. materials + textures --------------------------------------
        let source_textures = std::mem::take(&mut model.textures);
        let mut textures = Vec::with_capacity(source_textures.len());
        let mut texture_remap = vec![None; source_textures.len()];
        for (source_index, texture) in source_textures.into_iter().enumerate() {
            let expected = (texture.width as usize)
                .checked_mul(texture.height as usize)
                .and_then(|n| n.checked_mul(4));
            if texture.width == 0 || texture.height == 0 || expected != Some(texture.rgba.len()) {
                continue;
            }
            texture_remap[source_index] = Some(textures.len() as u32);
            textures.push(texture);
        }
        let mut materials: Vec<Material> = model
            .materials
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let texture = m
                    .texture
                    .and_then(|source| texture_remap.get(source).copied().flatten());
                material_from_data(MaterialId::from_index(i), m, texture)
            })
            .collect();
        let default_material = MaterialId::from_index(materials.len());
        materials.push(material_from_data(
            default_material,
            &MaterialData {
                name: "Default".into(),
                ..Default::default()
            },
            None,
        ));
        progress("materials", 0.03);

        // ---- 2. elements ---------------------------------------------------
        let mut elements: Vec<Element> = Vec::with_capacity(model.elements.len());
        for (i, e) in model.elements.iter().enumerate() {
            elements.push(Element {
                id: ElementId::from_index(i),
                name: if e.name.is_empty() {
                    format!("Element {i}")
                } else {
                    e.name.clone()
                },
                bounds: aabb_empty(),
                ranges: Vec::new(),
                triangle_count: 0,
            });
        }
        progress("elements", 0.06);

        // ---- 3. plan the batches ------------------------------------------
        // Cache each mesh's local bounds once; the planner needs a world
        // centroid per placement and nothing else.
        let mut mesh_bounds: Vec<Aabb> = Vec::with_capacity(model.meshes.len());
        for m in &model.meshes {
            let mut b = aabb_empty();
            for p in &m.positions {
                b = aabb_union_point(
                    &b,
                    Vec3f {
                        x: p[0],
                        y: p[1],
                        z: p[2],
                    },
                );
            }
            mesh_bounds.push(b);
        }

        let mut placements: Vec<Placement> = Vec::new();
        let mut next_part = 0u32;
        for (ei, e) in model.elements.iter().enumerate() {
            let mut element_part = 0u32;
            let element_world = Mat4f::mul(&normalise, &e.transform);
            for mref in &e.meshes {
                let Some(mesh) = model.meshes.get(mref.mesh.index()) else {
                    continue;
                };
                if mesh.indices.len() < 3 || mesh.positions.is_empty() {
                    continue;
                }
                let world = Mat4f::mul(&element_world, &mref.transform);
                let center = {
                    let b = mesh_bounds[mref.mesh.index()];
                    if aabb_is_empty(&b) {
                        Vec3f::default()
                    } else {
                        transform_point(&world, aabb_center(&b))
                    }
                };
                let override_material = mref.material;
                let groups: Vec<(MaterialId, u32, u32)> = if mesh.submeshes.is_empty() {
                    vec![(
                        override_material.unwrap_or(default_material),
                        0,
                        mesh.indices.len() as u32,
                    )]
                } else {
                    mesh.submeshes
                        .iter()
                        .map(|s| {
                            let m = override_material.unwrap_or(
                                if s.material.index() < materials.len() - 1 {
                                    s.material
                                } else {
                                    default_material
                                },
                            );
                            (m, s.first_index, s.index_count)
                        })
                        .collect()
                };
                for (material, first_index, index_count) in groups {
                    let end = ((first_index + index_count) as usize).min(mesh.indices.len());
                    let count = end.saturating_sub(first_index as usize) as u32;
                    if count < 3 {
                        continue;
                    }
                    placements.push(Placement {
                        element: ei as u32,
                        part: next_part,
                        element_part,
                        mesh: mref.mesh.0,
                        world,
                        material,
                        first_index,
                        index_count: count,
                        triangles: count / 3,
                        center,
                        batch: 0,
                    });
                    next_part += 1;
                    element_part += 1;
                }
            }
        }
        let plan = plan_batches(&mut placements, &materials);
        progress("batches", 0.12);

        // ---- 4. merge geometry --------------------------------------------
        let mut batches: Vec<RenderBatch> = plan
            .iter()
            .map(|(material, transparent)| RenderBatch {
                material: *material,
                transparent: *transparent,
                vertices: Vec::new(),
                indices: Vec::new(),
                element_ranges: Vec::new(),
                bounds: aabb_empty(),
            })
            .collect();

        let max_mesh_verts = model
            .meshes
            .iter()
            .map(|m| m.positions.len())
            .max()
            .unwrap_or(0);
        let mut world_pos: Vec<Vec3f> = vec![Vec3f::default(); max_mesh_verts];
        let mut world_nrm: Vec<Vec3f> = vec![Vec3f::default(); max_mesh_verts];
        let mut remap: Vec<u32> = vec![0; max_mesh_verts];
        let mut stamp: Vec<u32> = vec![0; max_mesh_verts];
        let mut cur_stamp: u32 = 0;
        let mut last_key: Option<(u32, Mat4f)> = None;
        let mut parts: Vec<Option<ScenePart>> = vec![None; next_part as usize];

        let total = placements.len().max(1);
        for (pi, p) in placements.iter().enumerate() {
            let mesh = &model.meshes[p.mesh as usize];
            let nv = mesh.positions.len();
            // Placements of the same mesh under the same transform are adjacent
            // after the plan sort only by luck, so recompute unless identical.
            if last_key.map_or(true, |(m, w)| m != p.mesh || w.v != p.world.v) {
                for (i, q) in mesh.positions.iter().enumerate() {
                    world_pos[i] = transform_point(
                        &p.world,
                        Vec3f {
                            x: q[0],
                            y: q[1],
                            z: q[2],
                        },
                    );
                }
                if mesh.normals.len() == nv {
                    for (i, q) in mesh.normals.iter().enumerate() {
                        world_nrm[i] = transform_dir(
                            &p.world,
                            Vec3f {
                                x: q[0],
                                y: q[1],
                                z: q[2],
                            },
                        )
                        .normalize();
                    }
                }
                last_key = Some((p.mesh, p.world));
            }
            let has_normals = mesh.normals.len() == nv;
            let has_uvs = mesh.uvs.len() == nv;
            let batch = &mut batches[p.batch as usize];
            let range_start = batch.indices.len() as u32;
            let mut el_bounds = aabb_empty();

            cur_stamp = cur_stamp.wrapping_add(1);
            if cur_stamp == 0 {
                stamp.iter_mut().for_each(|s| *s = 0);
                cur_stamp = 1;
            }
            let start = p.first_index as usize;
            let end = (start + p.index_count as usize).min(mesh.indices.len());
            let mut i = start;
            while i + 3 <= end {
                let (a, mut b, mut c) = (
                    mesh.indices[i] as usize,
                    mesh.indices[i + 1] as usize,
                    mesh.indices[i + 2] as usize,
                );
                i += 3;
                if a >= nv || b >= nv || c >= nv {
                    continue;
                }
                if flip_winding {
                    std::mem::swap(&mut b, &mut c);
                }
                if has_normals {
                    for &vi in &[a, b, c] {
                        if stamp[vi] != cur_stamp {
                            stamp[vi] = cur_stamp;
                            remap[vi] = batch.vertex_count() as u32;
                            let pos = world_pos[vi];
                            batch.push_vertex(&Vertex {
                                position: [pos.x, pos.y, pos.z],
                                element: p.element as f32,
                                normal: [world_nrm[vi].x, world_nrm[vi].y, world_nrm[vi].z],
                                _pad: 0.0,
                                uv: if has_uvs { mesh.uvs[vi] } else { [0.0, 0.0] },
                                _pad2: [0.0, 0.0],
                            });
                            batch.bounds = aabb_union_point(&batch.bounds, pos);
                            el_bounds = aabb_union_point(&el_bounds, pos);
                        }
                        batch.indices.push(remap[vi]);
                    }
                } else {
                    // No source normals: flat shading, so the three corners
                    // cannot be shared with the neighbouring triangle.
                    let flat = Vec3f::cross(
                        world_pos[b] - world_pos[a],
                        world_pos[c] - world_pos[a],
                    )
                    .normalize();
                    for &vi in &[a, b, c] {
                        let pos = world_pos[vi];
                        batch.indices.push(batch.vertex_count() as u32);
                        batch.push_vertex(&Vertex {
                            position: [pos.x, pos.y, pos.z],
                            element: p.element as f32,
                            normal: [flat.x, flat.y, flat.z],
                            _pad: 0.0,
                            uv: if has_uvs { mesh.uvs[vi] } else { [0.0, 0.0] },
                            _pad2: [0.0, 0.0],
                        });
                        batch.bounds = aabb_union_point(&batch.bounds, pos);
                        el_bounds = aabb_union_point(&el_bounds, pos);
                    }
                }
            }
            let range_count = batch.indices.len() as u32 - range_start;
            if range_count > 0 {
                batch.element_ranges.push(ElementRange {
                    element: ElementId::from_index(p.element as usize),
                    part: p.part,
                    first_index: range_start,
                    index_count: range_count,
                    draw_priority: 0,
                    coplanar_group: 0,
                });
                parts[p.part as usize] = Some(ScenePart {
                    id: p.part,
                    element: ElementId::from_index(p.element as usize),
                    element_part: p.element_part,
                    mesh: MeshId(p.mesh),
                    material: p.material,
                    batch: p.batch,
                    first_index: range_start,
                    index_count: range_count,
                    draw_priority: 0,
                    coplanar_group: 0,
                });
                let el = &mut elements[p.element as usize];
                el.ranges.push((p.batch, range_start, range_count));
                el.triangle_count += range_count / 3;
                el.bounds = aabb_union(&el.bounds, &el_bounds);
            }
            if pi % 256 == 0 {
                progress("geometry", 0.12 + 0.48 * (pi as f32 / total as f32));
            }
        }
        progress("overlap", 0.60);

        // ---- 4b. coplanar analysis + render-only metadata ----------------
        let mut parts: Vec<ScenePart> = parts
            .into_iter()
            .enumerate()
            .map(|(id, p)| {
                p.unwrap_or_else(|| {
                    let planned = placements
                        .iter()
                        .find(|p| p.part as usize == id)
                        .expect("part came from a placement");
                    ScenePart {
                        id: id as u32,
                        element: ElementId(planned.element),
                        element_part: planned.element_part,
                        mesh: MeshId(planned.mesh),
                        material: planned.material,
                        batch: planned.batch,
                        first_index: 0,
                        index_count: 0,
                        draw_priority: 0,
                        coplanar_group: 0,
                    }
                })
            })
            .collect();
        let element_of = |name: &str| elements.iter().position(|e| e.name == name);
        let apart: Vec<bool> = parts.iter().map(|p| copies.iter().any(|(copy, _)| *copy == elements[p.element.index()].name)).collect();
        let mut overlap_report = previous.filter(|old| overlap::same_analysis_inputs(old, &batches, &parts, &elements, &materials))
            .map(|old| old.overlap_report.clone())
            .unwrap_or_else(|| overlap::analyze(&batches, &parts, &elements, &materials, DEFAULT_COPLANAR_TOL_M, &apart, pool));
        overlap::resolve(&mut overlap_report, &mut parts, &materials);
        for (copy, original) in copies {
            let (Some(copy), Some(original)) = (element_of(copy), element_of(original)) else { continue };
            let (priority, group) = parts.iter().filter(|p| p.element.index() == original).map(|p| (p.draw_priority, p.coplanar_group)).max().unwrap_or((0, 0));
            for p in parts.iter_mut().filter(|p| p.element.index() == copy) {
                p.draw_priority = priority;
                p.coplanar_group = group;
            }
        }
        for batch in &mut batches {
            for range in &mut batch.element_ranges {
                let Some(part) = parts.get(range.part as usize) else {
                    continue;
                };
                range.draw_priority = part.draw_priority;
                range.coplanar_group = part.coplanar_group;
                // A placement owns its vertices (the remap stamp resets per
                // placement), so only padding changes; position/index/query
                // data stays byte-for-byte canonical.
                let start = range.first_index as usize;
                let end = (start + range.index_count as usize).min(batch.indices.len());
                for &vertex in &batch.indices[start..end] {
                    let o = vertex as usize * VERTEX_STRIDE;
                    batch.vertices[o + 7] = part.draw_priority as f32;
                    batch.vertices[o + 10] = part.coplanar_group as f32;
                }
            }
        }
        progress("overlap", 0.62);

        // ---- 5. bounds + spatial index --------------------------------------
        let mut bounds = aabb_empty();
        for b in &batches {
            bounds = aabb_union(&bounds, &b.bounds);
        }
        let bvh = Bvh::build_on(&batches, pool, &mut |f| progress("bvh", 0.64 + 0.34 * f));

        let stats = SceneStats {
            elements: elements.len() as u32,
            elements_with_geometry: elements.iter().filter(|e| e.has_geometry()).count() as u32,
            triangles: batches.iter().map(|b| b.triangle_count() as u32).sum(),
            vertices: batches.iter().map(|b| b.vertex_count() as u32).sum(),
            batches: batches.len() as u32,
            materials: materials.len() as u32,
            textures: textures.len() as u32,
            bvh_nodes: bvh.node_count() as u32,
            build_ms: t0.elapsed().as_secs_f32() * 1000.0,
            geometry_bytes: batches
                .iter()
                .map(|b| (b.vertices.len() * 4 + b.indices.len() * 4) as u64)
                .sum::<u64>(),
        };

        let build_generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut scene = Scene {
            name: model.name,
            source_path: model.source_path,
            units: model.units,
            elements,
            materials,
            batches,
            parts,
            overlap_report,
            bounds,
            bvh,
            stats,
            generation: build_generation,
            textures,
            prepared: None,
        };
        if !scene.is_empty() {
            progress("prepare", 1.0);
            match makepad_render::PreparedStaticPreview::prepare(crate::viewport::pack::pack_scene(&scene, pool)) {
                Ok(p) => scene.prepared = Some(Arc::new(PreparedModel(p))),
                Err(e) => makepad_widgets_core::log!("fab scene: the lit model could not be prepared: {e}"),
            }
        }
        progress("done", 1.0);
        scene
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    pub fn element(&self, id: ElementId) -> Option<&Element> {
        self.elements.get(id.index())
    }

    /// Element `id` is a part drawn from its distance field (no triangles): `bounds` is its
    /// box, for the selection, the framing and the scene's bounds.
    pub fn set_field_bounds(&mut self, id: ElementId, bounds: Aabb) {
        if let Some(e) = self.elements.get_mut(id.index()) {
            e.bounds = bounds;
            self.bounds = aabb_union(&self.bounds, &bounds);
        }
    }

    pub fn material(&self, id: MaterialId) -> Option<&Material> {
        self.materials.get(id.index())
    }

    /// Bounds of a set of elements (own geometry only). `None` if none of
    /// them has geometry.
    pub fn bounds_of(&self, ids: impl IntoIterator<Item = ElementId>) -> Option<Aabb> {
        let mut b = aabb_empty();
        for id in ids {
            if let Some(e) = self.element(id) {
                if !aabb_is_empty(&e.bounds) {
                    b = aabb_union(&b, &e.bounds);
                }
            }
        }
        if aabb_is_empty(&b) {
            None
        } else {
            Some(b)
        }
    }

    // ---- queries -------------------------------------------------------

    /// **The** pick: closest visible hit along `ray`, honouring hidden /
    /// isolated visibility and the section half-spaces, so a click always
    /// selects what is on screen.
    pub fn pick(&self, ray: &Ray, state: &SceneState) -> Option<RayHit> {
        if self.batches.is_empty() {
            return None;
        }
        let visible = |e: ElementId| state.is_visible(e);
        if state.section.is_inactive() {
            return self.bvh.raycast(&self.batches, ray, &visible);
        }
        let keeps = |p: Vec3f| state.section.keeps(p);
        self.bvh.raycast_opt(
            &self.batches,
            ray,
            &PickOptions {
                visible: &visible,
                accept: Some(&keeps),
                max_t: f32::INFINITY,
                cull_backfaces: false,
            },
        )
    }

    /// Elements whose world bounds intersect the frustum and that are visible.
    /// The renderer culls by batch; this is what the tools count.
    pub fn visible_elements(
        &self,
        frustum: &Frustum,
        state: &SceneState,
        out: &mut Vec<ElementId>,
    ) {
        self.bvh.frustum_elements(&self.batches, frustum, out);
        out.retain(|e| state.is_visible(*e));
    }
}

/// Assign every placement a batch, keyed by (material × spatial cell), and
/// return the batch descriptors in draw order (opaque first, transparent last).
fn plan_batches(
    placements: &mut [Placement],
    materials: &[Material],
) -> Vec<(MaterialId, bool)> {
    // Per material: how many triangles, and where they are.
    let mut tris: HashMap<u32, (usize, Aabb)> = HashMap::new();
    for p in placements.iter() {
        let e = tris
            .entry(p.material.0)
            .or_insert((0, aabb_empty()));
        e.0 += p.triangles as usize;
        e.1 = aabb_union_point(&e.1, p.center);
    }
    // Grid divisions per material: enough cells that a batch is roughly
    // TARGET_BATCH_TRIANGLES, and never more than 6 per axis (216 cells).
    let grid: HashMap<u32, (u32, Aabb)> = tris
        .iter()
        .map(|(m, (t, b))| {
            let want = (*t as f32 / TARGET_BATCH_TRIANGLES as f32).ceil().max(1.0);
            let n = (want.cbrt().ceil() as u32).clamp(1, 6);
            (*m, (n, *b))
        })
        .collect();

    let cell_of = |p: &Placement| -> u32 {
        let Some((n, b)) = grid.get(&p.material.0) else {
            return 0;
        };
        if *n <= 1 || aabb_is_empty(b) {
            return 0;
        }
        let e = b.max - b.min;
        let f = |v: f32, lo: f32, ext: f32| -> u32 {
            if ext <= 1e-6 {
                0
            } else {
                (((v - lo) / ext * *n as f32) as i32).clamp(0, *n as i32 - 1) as u32
            }
        };
        let (x, y, z) = (
            f(p.center.x, b.min.x, e.x),
            f(p.center.y, b.min.y, e.y),
            f(p.center.z, b.min.z, e.z),
        );
        (z * n * n) + (y * n) + x
    };

    let transparent_of = |m: MaterialId| -> bool {
        materials
            .get(m.index())
            .map(|mm| mm.transparent)
            .unwrap_or(false)
    };

    // Sort into draw order: opaque first, then by material, then by cell. That
    // is also the emission order, so each batch's element ranges come out
    // sorted by `first_index` and `element_of_triangle` can binary-search.
    let mut keyed: Vec<(bool, u32, u32, usize)> = placements
        .iter()
        .enumerate()
        .map(|(i, p)| (transparent_of(p.material), p.material.0, cell_of(p), i))
        .collect();
    keyed.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));

    let mut descriptors: Vec<(MaterialId, bool)> = Vec::new();
    let mut cur_key: Option<(bool, u32, u32)> = None;
    let mut cur_vertices = 0usize;
    let mut assign: Vec<u32> = vec![0; placements.len()];
    for (transparent, material, cell, i) in keyed {
        let key = (transparent, material, cell);
        // Upper bound on the vertices this placement adds.
        let need = placements[i].index_count as usize;
        let split = match cur_key {
            Some(k) if k == key => cur_vertices + need > MAX_BATCH_VERTICES,
            _ => true,
        };
        if split {
            descriptors.push((MaterialId(material), transparent));
            cur_key = Some(key);
            cur_vertices = 0;
        }
        cur_vertices += need;
        assign[i] = descriptors.len() as u32 - 1;
    }
    for (i, p) in placements.iter_mut().enumerate() {
        p.batch = assign[i];
    }
    // Emission must follow the same order the plan sorted into.
    placements.sort_by_key(|p| p.batch);
    descriptors
}

fn material_from_data(id: MaterialId, m: &MaterialData, texture: Option<u32>) -> Material {
    Material {
        id,
        name: m.name.clone(),
        base_color: m.base_color,
        metallic: m.metallic,
        roughness: m.roughness,
        emissive: m.emissive,
        ior: m.ior,
        transmission: m.transmission,
        double_sided: m.double_sided,
        transparent: m.base_color[3] < 0.999 || m.transmission > 0.001,
        texture,
    }
}

/// Source → scene: scale to meters and rotate Y-up sources to Z-up.
fn normalisation_matrix(up: UpAxis, scale: f32) -> Mat4f {
    let s = Mat4f {
        v: [
            scale, 0.0, 0.0, 0.0, 0.0, scale, 0.0, 0.0, 0.0, 0.0, scale, 0.0, 0.0, 0.0, 0.0, 1.0,
        ],
    };
    match up {
        UpAxis::Z => s,
        // Y-up → Z-up: (x, y, z) → (x, -z, y)
        UpAxis::Y => {
            let r = Mat4f {
                v: [
                    1.0, 0.0, 0.0, 0.0, // col 0
                    0.0, 0.0, 1.0, 0.0, // col 1: y → z
                    0.0, -1.0, 0.0, 0.0, // col 2: z → -y
                    0.0, 0.0, 0.0, 1.0,
                ],
            };
            Mat4f::mul(&r, &s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::state::{SectionPlane, SectionState};
    use makepad_math::Plane;

    #[test]
    fn the_parts_fixture_builds() {
        let model = crate::model::fixture::parts();
        let n = model.elements.len();
        let scene = Scene::from_model(model, &mut |_| {});
        assert!(scene.stats.triangles > 0);
        assert!(!aabb_is_empty(&scene.bounds));
        assert!(scene.bounds.max.z > scene.bounds.min.z);
        assert_eq!(scene.elements.len(), n);
        assert!(scene.elements.iter().all(|e| e.has_geometry()));
        let indices: usize = scene.batches.iter().map(|b| b.indices.len()).sum();
        assert_eq!(indices as u32, scene.stats.triangles * 3);
    }

    #[test]
    fn y_up_source_becomes_z_up() {
        let mut model = crate::model::fixture::parts();
        model.up_axis = UpAxis::Y;
        let scene = Scene::from_model(model, &mut |_| {});
        let ext = crate::model::bounds::aabb_extent(&scene.bounds);
        assert!(ext.z > 0.0);
    }

    #[test]
    fn shared_vertices_are_not_duplicated() {
        // Every part in the fixture shares its vertices between triangles;
        // with per-placement de-duplication the vertex count must stay far
        // below 3 × triangles.
        let scene = Scene::from_model(crate::model::fixture::parts(), &mut |_| {});
        assert!(
            scene.stats.vertices < scene.stats.triangles * 3,
            "{} verts for {} tris",
            scene.stats.vertices,
            scene.stats.triangles
        );
    }

    #[test]
    fn overlap_resolution_only_changes_render_padding() {
        use crate::model::model::{ElementData, MeshData, MeshRef};

        let source_positions = vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
        let model = ModelData {
            name: "canonical overlap".into(),
            meshes: vec![MeshData {
                positions: source_positions.clone(),
                indices: vec![0, 1, 2],
                ..Default::default()
            }],
            elements: vec![
                ElementData {
                    id: ElementId(0),
                    name: "first face".into(),
                    meshes: vec![MeshRef::identity(MeshId(0))],
                    ..Default::default()
                },
                ElementData {
                    id: ElementId(1),
                    name: "later face".into(),
                    meshes: vec![MeshRef::identity(MeshId(0))],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let scene = Scene::from_model(model, &mut |_| {});
        assert_eq!(scene.overlap_report.pairs.len(), 1);
        let overlap = &scene.overlap_report.pairs[0];
        assert!(overlap.draw_priority_b > overlap.draw_priority_a);

        for part in &scene.parts {
            let batch = &scene.batches[part.batch as usize];
            let tri = &batch.indices
                [part.first_index as usize..(part.first_index + part.index_count) as usize];
            let actual: Vec<[f32; 3]> = tri
                .iter()
                .map(|&i| batch.vertex(i).position)
                .collect();
            assert_eq!(actual, source_positions, "canonical triangle was moved");
        }
        let ray = Ray::new(
            Vec3f { x: 0.2, y: 0.2, z: 1.0 },
            Vec3f { x: 0.0, y: 0.0, z: -1.0 },
        );
        let hit = scene.pick(&ray, &SceneState::default()).expect("canonical pick");
        assert!((hit.point.z).abs() < 1.0e-7, "render bias leaked into picking");
        assert!((crate::model::query::polygon_area(&[
            Vec3f { x: 0.0, y: 0.0, z: 0.0 },
            Vec3f { x: 1.0, y: 0.0, z: 0.0 },
            Vec3f { x: 0.0, y: 1.0, z: 0.0 },
        ]) - 0.5)
            .abs()
            < 1.0e-7);
    }

    #[test]
    fn a_copy_stays_out_of_the_overlap_pass_and_takes_its_originals_priority() {
        use crate::model::model::{ElementData, MeshData, MeshRef};

        let face = |id: u32, name: &str| ElementData { id: ElementId(id), name: name.into(), meshes: vec![MeshRef::identity(MeshId(0))], ..Default::default() };
        let model = ModelData {
            name: "a copy".into(),
            meshes: vec![MeshData { positions: vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]], indices: vec![0, 1, 2], ..Default::default() }],
            elements: vec![face(0, "original"), face(1, "neighbour"), face(2, "copy")],
            ..Default::default()
        };
        let pairs = |scene: &Scene| {
            let mut p: Vec<[String; 2]> = scene.overlap_report.pairs.iter().map(|r| {
                let mut two = [r.element_a_name.clone(), r.element_b_name.clone()];
                two.sort();
                two
            }).collect();
            p.sort();
            p
        };
        // three faces in one place: each two of them pair
        assert_eq!(pairs(&Scene::from_model(model.clone(), &mut |_| {})).len(), 3);
        let scene = Scene::from_model_reusing(model, None, &[("copy".into(), "original".into())], None, &mut |_, _| {});
        assert_eq!(pairs(&scene), [["neighbour".to_string(), "original".to_string()]], "the copy pairs with neither");
        let of = |name: &str| {
            let e = scene.elements.iter().position(|e| e.name == name).unwrap();
            scene.parts.iter().find(|p| p.element.index() == e).map(|p| (p.draw_priority, p.coplanar_group)).unwrap()
        };
        assert_ne!(of("original").1, 0, "the original and its neighbour are a coplanar group");
        assert_eq!(of("copy"), of("original"), "the copy draws as its original does");
    }

    #[test]
    fn element_ranges_stay_sorted_per_batch() {
        let scene = Scene::from_model(crate::model::fixture::parts(), &mut |_| {});
        for b in &scene.batches {
            let mut last = 0;
            for r in &b.element_ranges {
                assert!(r.first_index >= last, "ranges out of order");
                last = r.first_index + r.index_count;
            }
            assert_eq!(last as usize, b.indices.len());
            // Every triangle resolves back to its element.
            for t in [0u32, (b.triangle_count() as u32).saturating_sub(1)] {
                if b.triangle_count() > 0 {
                    assert!(b.element_of_triangle(t).is_some());
                }
            }
        }
    }

    #[test]
    fn transparent_batches_come_last() {
        let scene = Scene::from_model(crate::model::fixture::parts(), &mut |_| {});
        let mut seen_transparent = false;
        for b in &scene.batches {
            if b.transparent {
                seen_transparent = true;
            } else {
                assert!(!seen_transparent, "opaque batch after a transparent one");
            }
        }
    }

    #[test]
    fn pick_respects_visibility_and_sections() {
        let scene = Scene::from_model(crate::model::fixture::parts(), &mut |_| {});
        let c = aabb_center(&scene.bounds);
        let ray = Ray::new(
            Vec3f {
                x: c.x,
                y: c.y,
                z: scene.bounds.max.z + 50.0,
            },
            Vec3f {
                x: 0.0,
                y: 0.0,
                z: -1.0,
            },
        );
        let mut state = SceneState::default();
        let hit = scene.pick(&ray, &state).expect("top face hit");
        // Cut everything above the hit away: the pick must fall through to
        // whatever is below, never back to the removed top.
        state.section = SectionState {
            enabled: true,
            planes: vec![SectionPlane {
                plane: Plane {
                    a: 0.0,
                    b: 0.0,
                    c: -1.0,
                    d: hit.point.z - 0.05,
                },
                enabled: true,
            }],
            boxed: None,
            caps: true,
        };
        let under = scene.pick(&ray, &state);
        if let Some(u) = under {
            assert!(u.point.z <= hit.point.z - 0.04, "picked the clipped half");
        }
        // Hiding the element that was hit must expose something else or nothing.
        let mut hidden = SceneState::default();
        hidden.set_hidden(hit.element, true);
        let after = scene.pick(&ray, &hidden);
        assert!(after.map_or(true, |h| h.element != hit.element));
    }
}

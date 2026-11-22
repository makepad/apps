//! The product's immutable triangle snapshot and each Traced pane's accumulation.
//! No camera participates in the shared build key. All CPU geometry work runs on
//! the background pool; only publication and the tracer's public upload run on Cx.

use super::board_geom::{BOARD_VERTEX_FLOATS, BODY_VERTEX_FLOATS};
use super::board_scene::{prepare_scene, SceneBoardPrep};
use super::bodies::{actuator_offset, bed_look, body_look, body_primitives, generated_emitter, knob_angles, lens_emission, press_port, shaft_turn, xform_mat, BodyPrimitive};
use super::part_glow::{posed_triangle, scene_pose_under, side_share, PartGlowLayer, PipeGlow, TOP_NORMAL_Z};
use super::solder::{self, SolderJoint, SOLDER_BASE, SOLDER_METAL};
use crate::api::*;
use fab_elec_core::{CompRef, Sel};
use fab_elec_mech::scene::{ElecScene, MeshRef};
use fab_elec_view::ElecLive;
use fab_materials::part_look::PartClass;
use fab_elec_parts::BodyPort;
use makepad_raytrace::pack::PackedScene;
use makepad_raytrace::{Camera as TraceCamera, DisplayCurve, EnvironmentFrame, EnvironmentLight, Material, RayTracer, RenderSettings, SceneInput, Sun};
use makepad_widgets_core::makepad_platform::thread::{checkpoint, Class, TaskHandle};
use makepad_widgets_core::*;
use std::collections::HashMap;
use std::sync::Arc;

/// Fixed FNV-1a over explicit numeric bytes, independent of process seeds and pointers.
struct ContentHash(u64);

impl Default for ContentHash {
    fn default() -> Self { Self(0xcbf2_9ce4_8422_2325) }
}

impl ContentHash {
    fn word(&mut self, word: u64) {
        for byte in word.to_le_bytes() {
            self.0 = (self.0 ^ byte as u64).wrapping_mul(0x100_0000_01b3);
        }
    }

    fn floats(&mut self, values: &[f32]) {
        for value in values { self.word(value.to_bits() as u64); }
    }
}

#[derive(Clone)]
struct MeshPart {
    element: ElementId,
    pose: Mat4f,
    color: [f32; 4],
    glass: bool,
    case: bool,
}

impl MeshPart {
    fn of_under(state: &AppState, poses: PanePoses, part: &PlatePart) -> Option<Self> {
        let element = part.element?;
        if !state.is_visible(element) || state.is_ghost_under(poses, element) || state.drawn_by_link.contains(&element)
            || (state.parts_under_slice() && !part.insert)
            || state.embed_waits(element)
            || state.plate.part_of(element).is_some_and(|i| state.part_unposed(i))
        {
            return None;
        }
        Some(Self {
            element,
            pose: scene_pose_under(state, poses, element),
            color: part.color,
            glass: state.is_glass_under(poses, element),
            case: state.glass_parts.hosts.contains(&element),
        })
    }

    fn material(&self, scene: &Scene) -> Material {
        let mut material = Material {
            albedo: [self.color[0], self.color[1], self.color[2]],
            roughness: FILAMENT_ROUGHNESS,
            ..Default::default()
        };
        if self.glass {
            if self.case {
                // A case the Glass view shows through is no glass of its filament's colour: it
                // lets the view's share through untinted, as the raster draws it (`glass.rs`).
                // Tinted by a dark filament, each wall face passed 2.5% and the case traced black.
                material.albedo = [1.0; 3];
                material.transmission = super::glass::CASE_SEE_THROUGH;
                material.roughness = 0.0;
            } else if let Some(source) = scene.elements[self.element.index()].ranges.iter()
                .map(|&(batch, _, _)| &scene.materials[scene.batches[batch as usize].material.index()])
                .find(|material| material.transparent)
            {
                material.albedo = [source.base_color[0], source.base_color[1], source.base_color[2]];
                material.roughness = source.roughness;
                material.ior = source.ior;
                material.transmission = (1.0 - source.base_color[3]).max(source.transmission);
            }
            material.two_sided = true;
        }
        material
    }
}

/// The plate face the parts stand on, traced in the open-area albedo the realtime mirror term
/// uses (`bed_look`, its texture's mean) with the face's own roughness and metalness. Its quad
/// is one-sided: a view from under the bed sees through it, as the realtime view leaves the
/// floor out there.
#[derive(Clone)]
struct BedFace {
    element: ElementId,
    pose: Mat4f,
    material: Material,
}

impl BedFace {
    /// The plate face when it is shown. Its mesh and material come with the scene's generation.
    fn element(state: &AppState) -> Option<ElementId> {
        let face = state.scene.elements.iter().find(|e| e.name.strip_prefix(CONTEXT_PREFIX) == Some("plate-face"))?;
        state.is_visible(face.id).then_some(face.id)
    }

    fn of_under(state: &AppState, poses: PanePoses) -> Option<Self> {
        let scene = &state.scene;
        let face = &scene.elements[Self::element(state)?.index()];
        let &(batch, _, _) = face.ranges.first()?;
        let source = scene.materials.get(scene.batches.get(batch as usize)?.material.index())?;
        let material = Material {
            albedo: bed_look(state)?.albedo,
            roughness: source.roughness,
            metal: source.metallic,
            ..Default::default()
        };
        Some(Self { element: face.id, pose: scene_pose_under(state, poses, face.id), material })
    }
}

/// Independent simulation decisions: levels change group gains; held
/// component changes invalidate geometry and submit the existing pool job.
#[derive(Clone, Default)]
struct LivePublication {
    lights: Option<u64>,
    held: Option<Arc<Vec<CompRef>>>,
    /// The bench's turned places and the knobs' starts: a change turns shafts ([`knob_angles`]).
    knobs: Option<(Arc<Vec<(CompRef, f64)>>, Arc<fab_elec_bench::ValueIndex>)>,
}

#[derive(Debug, PartialEq, Eq)]
struct LiveChanges {
    gains: bool,
    pack: bool,
}

impl LivePublication {
    fn read(cx: &Cx) -> Self {
        cx.get_global_ref::<ElecLive>().map_or_else(Self::default, |live| Self {
            lights: Some(live.sim.lights), held: Some(live.sim.held.clone()),
            knobs: Some((live.sim.turned.clone(), live.sim.index.clone())),
        })
    }

    fn changes_since(&self, old: &Self) -> LiveChanges {
        let same_held = match (&self.held, &old.held) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        let same_knobs = match (&self.knobs, &old.knobs) {
            (Some((t, i)), Some((u, j))) => Arc::ptr_eq(t, u) && Arc::ptr_eq(i, j),
            (None, None) => true,
            _ => false,
        };
        LiveChanges { gains: self.lights != old.lights, pack: !same_held || !same_knobs }
    }
}

/// Read only LED rows, from the very texture DrawFabBodyGlow samples. The
/// `glow` interval means are not a substitute for paused/scrubbed part rows.
fn light_levels(cx: &mut Cx) -> Arc<HashMap<CompRef, f32>> {
    let Some((parts, index, comps)) = cx.get_global_ref::<ElecLive>()
        .and_then(|live| live.sim.parts.clone().map(|parts|
            (parts, live.sim.index.clone(), live.sim.glow_parts.clone())))
    else { return Arc::new(HashMap::new()) };
    let TextureFormat::VecRGBAf32 { data: Some(data), .. } = parts.get_format(cx)
    else { return Arc::new(HashMap::new()) };
    Arc::new(comps.iter().filter_map(|&comp| {
        let row = index.comp(comp)? as usize;
        Some((comp, *data.get(row * 4 + 1)?))
    }).collect())
}

#[derive(Clone)]
struct Lens {
    comp: Option<CompRef>,
    emit: [f32; 3],
}

fn group_gain(comp: CompRef, levels: &HashMap<CompRef, f32>, white_to_trace: f32) -> f32 {
    levels.get(&comp).copied().unwrap_or(0.0) * white_to_trace
}

struct ProductInputs {
    scene: Arc<Scene>,
    electronics: Option<Arc<ElecScene>>,
    soldered: bool,
    parts: Arc<[MeshPart]>,
    pipes: Option<Arc<[PipeGlow]>>,
    bed: Option<BedFace>,
    /// Each scene board's plate pose (scene mm to plate mm, by board index), as BoardLayer
    /// placed it.
    board_poses: Option<Vec<Mat4f>>,
    fields_not_traced: bool,
    live: LivePublication,
}

impl ProductInputs {
    /// O(parts), no allocation: selection, hover, light and camera changes are absent.
    fn key(state: &AppState, board_poses: Option<&[Mat4f]>, pipe_key: Option<u64>) -> u64 {
        let mut key = ContentHash::default();
        key.word(state.scene.generation);
        key.word(Arc::as_ptr(&state.scene) as usize as u64);
        key.word(u64::from(state.view.soldered));
        if let Some(pipes) = pipe_key { key.word(pipes); }
        match board_poses {
            Some(poses) => {
                key.word(poses.len() as u64 + 1);
                for pose in poses {
                    key.floats(&pose.v);
                }
            }
            None => key.word(0),
        }
        let poses = state.shared_poses();
        match BedFace::element(state) {
            Some(bed) => {
                key.word(bed.0 as u64 + 1);
                key.floats(&scene_pose_under(state, poses, bed).v);
            }
            None => key.word(0),
        }
        for part in &state.plate.parts {
            key.word(u64::from(part.sdf.is_some()));
            if let Some(part) = MeshPart::of_under(state, poses, part) {
                key.word(part.element.0 as u64);
                key.floats(&part.pose.v);
                key.floats(&part.color);
                key.word(u64::from(part.glass));
                key.word(u64::from(part.case));
            }
        }
        key.0
    }

    fn capture(state: &AppState, board_poses: Option<&[Mat4f]>, pipes: Option<&Arc<[PipeGlow]>>) -> Self {
        let mut fields_not_traced = false;
        let poses = state.shared_poses();
        let parts = state.plate.parts.iter().filter_map(|part| {
            let mesh = part.element.and_then(|id| state.scene.elements.get(id.index()))
                .is_some_and(|element| element.ranges.iter().any(|&(_, _, count)| count != 0));
            fields_not_traced |= part.sdf.is_some() && !mesh;
            mesh.then(|| MeshPart::of_under(state, poses, part)).flatten()
        }).collect::<Vec<_>>().into();
        Self {
            scene: state.scene.clone(),
            electronics: state.elec_scene.clone(),
            soldered: state.view.soldered,
            parts,
            pipes: pipes.cloned(),
            bed: BedFace::of_under(state, poses),
            board_poses: board_poses.map(<[Mat4f]>::to_vec),
            fields_not_traced,
            live: LivePublication::default(),
        }
    }
}

/// One generated mesh or a file's meshes with their material surfaces and lens flags.
type PreparedBody = Arc<[BodyPrimitive]>;

struct PreparedElectronics {
    source: Arc<ElecScene>,
    soldered: bool,
    boards: Vec<SceneBoardPrep>,
    bodies: Vec<Vec<PreparedBody>>,
    joints: Vec<Vec<SolderJoint>>,
    missing_bodies: bool,
    emitting_lens: bool,
}

impl PreparedElectronics {
    fn build(source: &Arc<ElecScene>, soldered: bool) -> Option<Self> {
        checkpoint().ok()?;
        // This is the board layer's preparation, including its faces and walls.
        // The GPU layer consumes its copy; it does not expose CPU mesh handles.
        let boards = prepare_scene(source);
        // Keyed as the raster layer keys its batches: a generated emitter splits off its top lens.
        let mut cache: Vec<(&MeshRef, PartClass, bool, Option<&BodyPort>, PreparedBody)> = Vec::new();
        let mut bodies = Vec::with_capacity(source.boards.len());
        let mut missing_bodies = false;
        let mut emitting_lens = false;
        for board in &source.boards {
            let mut placed = Vec::with_capacity(board.bodies.len());
            for body in &board.bodies {
                checkpoint().ok()?;
                let generated_emitter = generated_emitter(body);
                let meshes = if let Some((_, _, _, _, meshes)) = cache.iter().find(|(mesh, class, emitter, port, _)|
                    *mesh == &body.mesh && *class == body.class && *emitter == generated_emitter && *port == press_port(body)) {
                    meshes.clone()
                } else {
                    // Like BodyLayer, unsupported scripts have no invented geometry.
                    let built = body_primitives(body).unwrap_or_default();
                    missing_bodies |= built.is_empty();
                    let meshes: PreparedBody = built.into();
                    cache.push((&body.mesh, body.class, generated_emitter, press_port(body), meshes.clone()));
                    meshes
                };
                emitting_lens |= body.emitter.is_some() && meshes.iter().any(|prim| prim.lens);
                placed.push(meshes);
            }
            bodies.push(placed);
        }
        let joints = if soldered {
            solder::scene_joints(source, &mut |board, body| {
                let placed = &source.boards[board].bodies[body];
                solder::body_triangles(placed, bodies[board][body].iter().map(|prim| {
                    let (_, metal, _, _) = body_look(placed.surface.as_ref().or(prim.surface.as_ref()));
                    (&prim.mesh, metal >= 0.5)
                }))
            })?
        } else { Vec::new() };
        Some(Self { source: source.clone(), soldered, boards, bodies, joints, missing_bodies, emitting_lens })
    }
}

/// Retained triangles for one source stream, independent of its material.
struct GeometryBlock {
    hash: u64,
    mesh: SceneInput,
}

/// The content hash excludes the camera and environment: those are per pane.
pub(super) struct ProductScene {
    input: SceneInput,
    packed: Arc<PackedScene>,
    groups: Vec<CompRef>,
    hash: u64,
    geometry_hash: u64,
    blocks: Vec<Arc<GeometryBlock>>,
    electronics: Option<Arc<PreparedElectronics>>,
    fields_not_traced: bool,
    bodies_not_traced: bool,
    pub(super) emitting_lens: bool,
    geometry_rebuilt: bool,
}

impl ProductScene {
    fn build(inputs: ProductInputs, previous: Option<&Self>) -> Option<Self> {
        let electronics = match &inputs.electronics {
            Some(source) => Some(match previous.and_then(|p| p.electronics.as_ref()) {
                Some(prepared) if prepared.source.hash == source.hash && prepared.soldered == inputs.soldered => prepared.clone(),
                _ => Arc::new(PreparedElectronics::build(source, inputs.soldered)?),
            }),
            None => None,
        };
        let mut geometry = ContentHash::default();
        let mut materials = Vec::new();
        let mut blocks = Vec::new();
        let mut material_slots = Vec::new();
        let mut lenses = Vec::new();
        // Hash only referenced geometry, never source generations, addresses or colours.
        Self::streams(&inputs, electronics.as_deref(), |vertices, indices, stride, transform, scale, material, reuse_material, lens| {
            let mut block_hash = ContentHash::default();
            block_hash.word(indices.len() as u64);
            geometry.word(u64::from(reuse_material));
            block_hash.floats(&transform.v);
            block_hash.floats(&[scale]);
            for (at, &index) in indices.iter().enumerate() {
                if at % 4096 == 0 { checkpoint().ok()?; }
                let v = &vertices[index as usize * stride..][..stride];
                block_hash.floats(&v[..3]);
                block_hash.floats(&v[4..7]);
            }
            geometry.word(block_hash.0);
            let block = match previous.and_then(|p| p.blocks.get(blocks.len())) {
                Some(block) if block.hash == block_hash.0 => block.clone(),
                _ => {
                    let mut mesh = SceneInput::default();
                    append(&mut mesh, vertices, indices, stride, transform, scale, 0)?;
                    mesh.ensure_normals();
                    Arc::new(GeometryBlock { hash: block_hash.0, mesh })
                }
            };
            blocks.push(block);
            if let Some(lens) = lens { lenses.push((materials.len(), lens)); }
            if !reuse_material { materials.push(material); }
            material_slots.push((materials.len() - 1) as u32);
            Some(())
        })?;
        let geometry_hash = geometry.0;
        let geometry_rebuilt = previous.is_none_or(|p| p.geometry_hash != geometry_hash);
        let mut input = if let Some(previous) = previous.filter(|_| !geometry_rebuilt) {
            // Keep geometry and triangle/material ordering when only appearance changes.
            previous.input.clone()
        } else {
            let mut input = SceneInput { up: vec3(0.0, 0.0, 1.0), ..Default::default() };
            for (block, material) in blocks.iter().zip(material_slots) {
                checkpoint().ok()?;
                let mesh = &block.mesh;
                input.push_mesh(&mesh.positions, Some(&mesh.normals), None, &mesh.indices, material);
            }
            input
        };
        // All surfaces driven by one component share its sorted scene id.
        let mut groups: Vec<_> = lenses.iter().filter_map(|(_, lens)| lens.comp).collect();
        groups.sort_unstable();
        groups.dedup();
        for (slot, lens) in lenses {
            if let Some(comp) = lens.comp {
                let material = &mut materials[slot];
                material.light_group = groups.binary_search(&comp).unwrap() as u32 + 1;
                material.emission = lens.emit;
            }
        }
        input.materials = materials;
        let mut hash = ContentHash::default();
        hash.word(geometry_hash);
        for comp in &groups {
            hash.word(comp.board.0 as u64);
            hash.word(comp.comp.0 as u64);
        }
        for material in &input.materials {
            hash.floats(&material.albedo);
            hash.floats(&[material.roughness, material.metal, material.ior, material.transmission]);
            hash.floats(&material.emission);
            hash.word(u64::from(material.two_sided));
            hash.word(u64::from(material.light_group));
        }
        checkpoint().ok()?;
        let packed = match previous.filter(|p| p.hash == hash.0) {
            Some(previous) => previous.packed.clone(),
            None => Arc::new(PackedScene::pack(&input)),
        };
        checkpoint().ok()?;
        Some(Self {
            input,
            packed,
            groups,
            hash: hash.0,
            geometry_hash,
            blocks,
            fields_not_traced: inputs.fields_not_traced,
            bodies_not_traced: electronics.as_ref().is_some_and(|e| e.missing_bodies),
            emitting_lens: electronics.as_ref().is_some_and(|e| e.emitting_lens) || inputs.pipes.is_some(),
            electronics,
            geometry_rebuilt,
        })
    }

    fn streams(
        inputs: &ProductInputs,
        electronics: Option<&PreparedElectronics>,
        mut visit: impl FnMut(&[f32], &[u32], usize, &Mat4f, f32, Material, bool, Option<Lens>) -> Option<()>,
    ) -> Option<()> {
        for part in inputs.parts.iter() {
            checkpoint().ok()?;
            if let Some(pipe) = inputs.pipes.as_ref().and_then(|pipes| pipes.iter().find(|pipe| pipe.element == part.element)) {
                // Classify each triangle once in its posed Fab frame. Each band keeps one
                // material across batch ranges, with its own live LED emission.
                let mut bands: [Vec<(usize, Vec<u32>)>; 4] = Default::default();
                for &(batch, first, count) in &inputs.scene.elements[part.element.index()].ranges {
                    let mesh = &inputs.scene.batches[batch as usize];
                    let mut indices: [Vec<u32>; 4] = Default::default();
                    for (i, tri) in mesh.indices[first as usize..(first + count) as usize].chunks_exact(3).enumerate() {
                        if i % 1024 == 0 { checkpoint().ok()?; }
                        let (points, normal) = posed_triangle(&mesh.vertices, tri, &part.pose);
                        let band = if normal.z >= TOP_NORMAL_Z { 0 } else {
                            let z = (points[0].z + points[1].z + points[2].z) / 3.0;
                            1 + ((pipe.height(z) * 3.0) as usize).min(2)
                        };
                        indices[band].extend_from_slice(tri);
                    }
                    for (band, indices) in bands.iter_mut().zip(indices) {
                        if !indices.is_empty() { band.push((batch as usize, indices)); }
                    }
                }
                let shares = [1.0, side_share(1.0 / 6.0), side_share(0.5), side_share(5.0 / 6.0)];
                for (band, share) in bands.iter().zip(shares) {
                    if band.is_empty() { continue; }
                    let lens = Lens { comp: Some(pipe.comp), emit: pipe.emit.map(|v| v * share) };
                    let material = part.material(&inputs.scene);
                    for (i, (batch, indices)) in band.iter().enumerate() {
                        visit(&inputs.scene.batches[*batch].vertices, indices, VERTEX_STRIDE, &part.pose, 1.0,
                            material.clone(), i != 0, (i == 0).then(|| lens.clone()))?;
                    }
                }
                continue;
            }
            let mut reuse_material = false;
            for &(batch, first, count) in &inputs.scene.elements[part.element.index()].ranges {
                if count == 0 { continue; }
                let batch = &inputs.scene.batches[batch as usize];
                visit(&batch.vertices, &batch.indices[first as usize..(first + count) as usize],
                    VERTEX_STRIDE, &part.pose, 1.0, part.material(&inputs.scene), reuse_material, None)?;
                reuse_material = true;
            }
        }
        if let Some(bed) = &inputs.bed {
            let mut reuse_material = false;
            for &(batch, first, count) in &inputs.scene.elements[bed.element.index()].ranges {
                if count == 0 { continue; }
                let batch = &inputs.scene.batches[batch as usize];
                visit(&batch.vertices, &batch.indices[first as usize..(first + count) as usize],
                    VERTEX_STRIDE, &bed.pose, 1.0, bed.material.clone(), reuse_material, None)?;
                reuse_material = true;
            }
        }
        // Once a pack: each knob's turn, which its shaft takes as the raster's does.
        let knob_turns = inputs.live.knobs.as_ref().map_or_else(Vec::new, |(turned, index)| knob_angles(turned, index.knobs()));
        if let (Some(prepared), Some(source), Some(poses)) = (electronics, &inputs.electronics, &inputs.board_poses) {
            for (index, board) in source.boards.iter().enumerate() {
                checkpoint().ok()?;
                let board_pose = Mat4f::mul(poses.get(index)?, &xform_mat(&board.pose));
                let prep = &prepared.boards[index];
                // One material for each board, shared by its faces and walls.
                let mask = prep.look.mask;
                let material = Material { albedo: [mask[0], mask[1], mask[2]], roughness: prep.look.laminate[3], ..Default::default() };
                visit(&prep.faces.vertices, &prep.faces.indices, BOARD_VERTEX_FLOATS, &board_pose, 0.001, material.clone(), false, None)?;
                visit(&prep.edges.vertices, &prep.edges.indices, BOARD_VERTEX_FLOATS, &board_pose, 0.001, material, true, None)?;
                for (body, meshes) in board.bodies.iter().zip(&prepared.bodies[index]) {
                    let body_pose = Mat4f::mul(&board_pose, &xform_mat(&body.pose));
                    for BodyPrimitive { mesh, surface, lens, actuator, turn } in meshes.iter() {
                        let held = *actuator && matches!(&body.sel, Sel::Comp(comp)
                            if inputs.live.held.as_ref().is_some_and(|held| held.contains(comp)));
                        let pose = if held { Mat4f::mul(&body_pose, &actuator_offset(body)) } else { body_pose };
                        let angle = if !*turn { None } else { match &body.sel {
                            Sel::Comp(comp) => knob_turns.iter().find(|t| t.0 == *comp).map(|t| t.1),
                            _ => None,
                        } };
                        let pose = angle.map_or(pose, |angle| Mat4f::mul(&pose, &shaft_turn(body, angle)));
                        let lens = (*lens && body.emitter.is_some()).then(|| Lens {
                            comp: match &body.sel { Sel::Comp(comp) => Some(*comp), _ => None },
                            emit: lens_emission(body, mesh),
                        });
                        // The body's stated surface wins over its file's inferred one, as the
                        // realtime bodies draw it (`BodyUpload::build`).
                        let surface = body.surface.or(*surface);
                        let (base, metal, transmission, _) = body_look(surface.as_ref());
                        // The index the realtime body's reflectance comes from (`shade::shade`).
                        let ior = surface.as_ref().map_or_else(|| fab_materials::shade::unspecified().medium().ior, |s| s.medium().ior);
                        // Polished transmissive bodies are windows in the traced scene.
                        let roughness = if transmission > 0.0 && surface.as_ref().is_some_and(|surface|
                            matches!(surface.finish.name(), "polished_clear" | "spi_a2")) {
                            0.0
                        } else {
                            base[3]
                        };
                        let material = Material {
                            albedo: [base[0], base[1], base[2]], roughness, metal,
                            ior, transmission, two_sided: transmission > 0.0,
                            ..Default::default()
                        };
                        visit(&mesh.vertices, &mesh.indices, BODY_VERTEX_FLOATS, &pose, 0.001, material, false, lens)?;
                    }
                }
                if let Some(joints) = prepared.joints.get(index) {
                    let mut order: Vec<_> = (0..joints.len()).collect();
                    order.sort_unstable_by_key(|&i| joints[i].roughness.to_bits());
                    let mut previous_step = None;
                    for i in order {
                        checkpoint().ok()?;
                        let joint = &joints[i];
                        let body = &board.bodies[joint.body];
                        // Solder stays with the component body, never its held actuator offset.
                        let body_pose = Mat4f::mul(&board_pose, &xform_mat(&body.pose));
                        let pose = Mat4f::mul(&body_pose, &joint.body_from_joint);
                        let step = joint.roughness.to_bits();
                        let material = Material { albedo: SOLDER_BASE, roughness: joint.roughness, metal: SOLDER_METAL, ..Default::default() };
                        visit(&joint.mesh.mesh.vertices, &joint.mesh.mesh.indices, BODY_VERTEX_FLOATS, &pose, 0.001,
                            material, previous_step == Some(step), None)?;
                        previous_step = Some(step);
                    }
                }
            }
        }
        Some(())
    }
}

fn append(input: &mut SceneInput, vertices: &[f32], indices: &[u32], stride: usize, pose: &Mat4f, scale: f32, material: u32) -> Option<()> {
    let normal = pose.invert().transpose();
    let m = &pose.v;
    let reflected = vec3(m[0], m[1], m[2]).dot(Vec3f::cross(vec3(m[4], m[5], m[6]), vec3(m[8], m[9], m[10]))) < 0.0;
    for (at, tri) in indices.chunks_exact(3).enumerate() {
        if at % 1024 == 0 { checkpoint().ok()?; }
        let tri = if reflected { [tri[0], tri[2], tri[1]] } else { [tri[0], tri[1], tri[2]] };
        for index in tri {
            let v = &vertices[index as usize * stride..][..stride];
            let p = pose.transform_vec4(vec4(v[0], v[1], v[2], 1.0));
            let n = normal.transform_vec4(vec4(v[4], v[5], v[6], 0.0));
            let n = vec3(n.x, n.y, n.z);
            let n = if n.length() > 0.0 { n.normalize() } else { n };
            input.indices.push(input.positions.len() as u32);
            input.positions.push([p.x * scale, p.y * scale, p.z * scale]);
            input.normals.push([n.x, n.y, n.z]);
        }
        input.tri_material.push(material);
        input.tri_priority.push(0);
        input.tri_coplanar_group.push(0);
    }
    Some(())
}

/// One cancellable, latest-wins job for all panes. No task or allocation in Raster mode.
#[derive(Default)]
pub(crate) struct ProductSceneCache {
    requested: Option<u64>,
    live: LivePublication,
    levels: Arc<HashMap<CompRef, f32>>,
    white_to_trace: f32,
    /// Retain the last requested Arc, so pointer reuse cannot hide a pose update.
    requested_electronics: Option<Arc<ElecScene>>,
    pending: Option<TaskHandle<Option<ProductScene>>>,
    pub(super) scene: Option<Arc<ProductScene>>,
    pub(super) failed: bool,
    pub(super) fields_not_traced: bool,
    board_pose_pending: bool,
    pub(super) product_scene_builds: u64,
}

impl Drop for ProductSceneCache {
    fn drop(&mut self) {
        if let Some(task) = &self.pending { task.cancel(); }
    }
}

impl ProductSceneCache {
    pub(super) fn ready(&self) -> bool {
        self.pending.as_ref().is_some_and(TaskHandle::is_finished)
    }

    pub(super) fn stale(&self) -> bool { self.pending.is_some() || self.failed || self.board_pose_pending }

    pub(super) fn sync(&mut self, cx: &mut Cx, state: &AppState, board_poses: Option<&[Mat4f]>, white_to_trace: f32, pipes: &PartGlowLayer) {
        let live = LivePublication::read(cx);
        let changes = live.changes_since(&self.live);
        if changes.gains { self.levels = light_levels(cx); }
        self.live = live;
        self.white_to_trace = white_to_trace;
        let (pipe_key, pipe_glows) = if pipes.seated_under(state, state.shared_poses()) {
            (pipes.key(), pipes.pipes.as_ref())
        } else {
            (None, None)
        };
        let key = ProductInputs::key(state, board_poses, pipe_key);
        let same_electronics = match (&self.requested_electronics, &state.elec_scene) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        // Test the latest inputs BEFORE consuming any old completion.
        if self.requested != Some(key) || !same_electronics || changes.pack {
            if let Some(task) = self.pending.take() { task.cancel(); }
            let mut inputs = ProductInputs::capture(state, board_poses, pipe_glows);
            inputs.live = self.live.clone();
            self.fields_not_traced = inputs.fields_not_traced;
            self.board_pose_pending = inputs.electronics.is_some() && inputs.board_poses.is_none();
            self.requested = Some(key);
            self.requested_electronics = state.elec_scene.clone();
            self.failed = false;
            let previous = self.scene.clone();
            match cx.task_pool().submit_named(Class::Background, "fab-product-scene", move || {
                ProductScene::build(inputs, previous.as_deref())
            }) {
                Ok(task) => self.pending = Some(task),
                Err(error) => {
                    self.failed = true;
                    log!("fab product scene: {error}");
                }
            }
        }
        if let Some(result) = self.pending.as_mut().and_then(TaskHandle::try_take) {
            self.pending = None;
            match result {
                Ok(Some(scene)) => {
                    if scene.geometry_rebuilt {
                        self.product_scene_builds += 1;
                        log!("fab product_scene_builds={} triangles={} hash={:016x}", self.product_scene_builds, scene.input.tri_count(), scene.hash);
                    }
                    self.scene = Some(Arc::new(scene));
                }
                Ok(None) => self.failed = true,
                Err(error) => {
                    self.failed = true;
                    log!("fab product scene: {error}");
                }
            }
        }
    }
}

/// Scheduling and restart accounting without Cx or GPU resources.
pub(super) struct TraceSchedule {
    pub target: u32,
    pub samples: u32,
    pub trace_restarts: u64,
    camera: Option<TraceCamera>,
    done: bool,
}

impl Default for TraceSchedule {
    fn default() -> Self {
        Self { target: 256, samples: 0, trace_restarts: 0, camera: None, done: false }
    }
}

impl TraceSchedule {
    fn set_camera(&mut self, camera: TraceCamera) -> bool {
        if self.camera.as_ref() == Some(&camera) { return false; }
        self.camera = Some(camera);
        self.restart();
        true
    }

    fn restart(&mut self) {
        self.samples = 0;
        self.done = false;
        self.trace_restarts += 1;
    }

    fn progress(&mut self, samples: f32, done: bool) {
        self.samples = (samples.max(0.0) as u32).min(self.target);
        self.done = done || self.samples >= self.target;
    }

    pub(super) fn wants_frame(&self, ready: bool, stale: bool) -> bool {
        ready && !stale && !self.done
    }

    /// Whether this frame traces: every frame at the tracer's full budget until the target, then
    /// none. A pointer or touch holding the view pauses it. (One frame in ten after 30 s left a
    /// close-up of a board at about 105 of 256 samples, a quarter of an hour from its target.)
    fn advance(&self, ready: bool, stale: bool, interacting: bool) -> bool {
        self.wants_frame(ready, stale) && !interacting
    }
}

/// Each pane retains its own texture, accumulation and scheduling state.
pub(super) struct TracedPane {
    tracer: RayTracer,
    pub(super) schedule: TraceSchedule,
    loaded: Option<u64>,
    sun: Option<Sun>,
    /// The environment map lighting it (its address) and where the map hangs.
    environment: Option<(usize, EnvironmentFrame)>,
    size: (usize, usize),
    // An owned pass anchors the tracer's private pass chain. Detaching it on
    // idle/stale frames prevents retained trace draws from replaying on repaints.
    anchor: DrawPass,
    anchor_list: DrawList,
    anchor_texture: Texture,
    pub(super) stale: bool,
    lights: Option<Option<u64>>,
    sent_gains: Vec<f32>,
    gain_white: Option<u32>,
    observed_lights: Option<(Option<u64>, u64)>,
    gain_frames: std::collections::VecDeque<(u64, u64)>,
    gain_log_at: Option<f64>,
    logged_uploads: u64,
    /// The trace's rate log (`log_rate`): when it last logged, the frames this pane had drawn and
    /// the tracer's, the samples, and the frames it traced while busy, by then.
    rate_log: Option<(f64, u64, u32, f32, u64)>,
    /// Frames this pane drew while it traced (fresh frames that advanced the trace).
    traced_frames: u64,
    /// Of those, the frames traced while the person acted (`RenderSettings::interactive`) or the
    /// device played.
    busy_frames: u64,
    /// The person acted (a pointer, touch or key event) since the pane last drew a trace frame.
    acted: bool,
    max_latency_frames: u64,
    chip_at: f64,
    chip_timer: Option<Timer>,
    last_draw: Option<u64>,
    recorded_at: Option<u64>,
}

/// While the person does not act, the most a trace step takes, in display intervals: how long
/// the first frame after they start to act may wait behind it (`RenderSettings::frame_budget`).
const IDLE_STEP_INTERVALS: f64 = 3.0;

impl TracedPane {
    pub(super) fn new(vm: &mut ScriptVm) -> Self {
        let mut tracer = RayTracer::new(vm);
        tracer.set_settings(RenderSettings {
            target_spp: 256, progressive: true, denoise: true,
            ..Default::default()
        });
        let cx = vm.cx_mut();
        Self {
            tracer, schedule: TraceSchedule::default(), loaded: None, sun: None, environment: None,
            size: (0, 0), anchor: DrawPass::new(cx), anchor_list: DrawList::new(cx),
            anchor_texture: Texture::new_with_format(cx, TextureFormat::RenderBGRAu8 {
                size: TextureSize::Fixed { width: 1, height: 1 }, initial: true,
            }),
            stale: true, lights: None, sent_gains: Vec::new(), gain_white: None,
            observed_lights: None, gain_frames: Default::default(), gain_log_at: None,
            logged_uploads: 0, rate_log: None, traced_frames: 0, busy_frames: 0, acted: false, max_latency_frames: 0, chip_at: f64::NEG_INFINITY, chip_timer: None, last_draw: None, recorded_at: None,
        }
    }

    pub(super) fn texture(&self) -> Option<&Texture> { self.tracer.view_texture() }

    /// Every 2 s while it traces, where a trace's time goes: samples a second, the frames this
    /// pane traced in and the tracer's own a second, the share of them traced while busy
    /// (acting or playing), the work of its last frame (tiles of what edge, at which rung of the
    /// resolution ladder), that frame's GPU time against its slice, the tile count the steps
    /// climb at with the median and p90 of its times, and the steps that overran their slice so
    /// far.
    /// Slow sampling with few frames is the pane's frame requests; with full frames, the tracer.
    /// Cost: a compare a traced frame, a line every 2 s.
    fn log_rate(&mut self, pane: PaneId, now: f64) {
        let stats = &self.tracer.stats;
        let snapshot = (now, self.traced_frames, stats.frames, stats.spp, self.busy_frames);
        let (at, frames, tracer_frames, spp, busy) = *self.rate_log.get_or_insert(snapshot);
        let dt = now - at;
        // a restart begins a new window
        if stats.spp < spp || stats.frames < tracer_frames {
            self.rate_log = Some(snapshot);
            return;
        }
        if dt < 2.0 { return; }
        let traced = self.traced_frames - frames;
        log!("fab pane {} tracing {}x{}: {:.2} samples/s, {:.1} frames/s ({:.1} tracer frames/s, {:.0}% busy), {} tiles of {} px at rung {}, gpu {:.2} of {:.2} ms, steps of {} tiles take {:.1} ms (p90 {:.1}), {} late slices",
            pane.0, stats.width, stats.height, (stats.spp - spp) as f64 / dt, traced as f64 / dt,
            stats.frames.saturating_sub(tracer_frames) as f64 / dt, 100.0 * (self.busy_frames - busy) as f64 / traced.max(1) as f64,
            stats.tiles, stats.tile_edge, stats.rung_shift, stats.gpu_time_ms, stats.gpu_budget_ms,
            stats.step_tiles, stats.step_median_ms, stats.step_p90_ms, stats.late_slices);
        self.rate_log = Some(snapshot);
    }

    /// The realtime composite's display transform (`display_transform`): the trace shows
    /// exposed, balanced and graded as the realtime picture of the same view does, through the
    /// same AgX look. Display-only: the samples stay.
    pub(super) fn set_display(&mut self, exposure: f32, white_balance: [f32; 3], contrast: f32) {
        let curve = DisplayCurve::AgxLook { contrast, saturation: 1.0 };
        let s = &self.tracer.settings;
        if s.exposure == exposure && s.white_balance == white_balance && s.curve == curve { return; }
        let mut s = s.clone();
        s.exposure = exposure;
        s.white_balance = white_balance;
        s.curve = curve;
        self.tracer.set_settings(s);
    }

    pub(super) fn live_changed(&self, cx: &Cx, product: Option<&ProductSceneCache>) -> bool {
        product.and_then(|product| product.scene.as_ref()).is_some_and(|scene| scene.emitting_lens)
            && self.lights != Some(cx.get_global_ref::<ElecLive>().map(|live| live.sim.lights))
    }

    pub(super) fn detach(&self, cx: &mut Cx) { self.anchor.detach(cx); }

    pub(super) fn close(&mut self, cx: &mut Cx) {
        self.detach(cx);
        if let Some(timer) = self.chip_timer.take() { cx.stop_timer(timer); }
    }

    fn note_gain_paint(&mut self, cx: &Cx) {
        let shown = self.tracer.presented_gain_upload(cx);
        while self.gain_frames.front().is_some_and(|(upload, _)| *upload <= shown) {
            let (upload, arrival) = self.gain_frames.pop_front().unwrap();
            if upload == shown {
                self.max_latency_frames = self.max_latency_frames.max(frame_trace::presented_frames().saturating_sub(arrival));
            }
        }
    }

    pub(super) fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if matches!(event, Event::MouseMove(_) | Event::MouseDown(_) | Event::MouseUp(_) | Event::Scroll(_)
            | Event::TouchUpdate(_) | Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_))
        {
            self.acted = true;
        }
        let lights = cx.get_global_ref::<ElecLive>().map(|live| live.sim.lights);
        if self.observed_lights.map(|(id, _)| id) != Some(lights) {
            self.observed_lights = Some((lights, frame_trace::presented_frames()));
        }
        self.note_gain_paint(cx);
        // A batch must paint before its parent is retired. Subsequent window
        // repaints must not replay the last recorded batch behind the host's count.
        if self.recorded_at.is_some_and(|serial| cx.passes[self.anchor.draw_pass_id()].painted_serial > serial) {
            self.detach(cx);
            self.recorded_at = None;
        }
        if self.chip_timer.is_some_and(|timer| timer.is_event(event).is_some()) {
            self.chip_timer = None;
            return true;
        }
        false
    }

    pub(super) fn invalidate(&mut self, pane: PaneId, why: &str) {
        self.tracer.restart();
        self.restart(pane, why);
    }

    fn restart(&mut self, pane: PaneId, why: &str) {
        self.schedule.restart();
        log!("fab pane {} trace_restarts={} ({why})", pane.0, self.schedule.trace_restarts);
    }

    /// `environment`: the map that lights the scene in place of the sky and sun (the studio's,
    /// turned as the realtime studio turns it), if any.
    pub(super) fn draw(&mut self, cx: &mut Cx2d, state: &mut AppState, pane: PaneId, rect: Rect, product: &ProductSceneCache, environment: Option<(Arc<EnvironmentLight>, EnvironmentFrame)>) -> bool {
        let want = state.view.traced_detail == TracedDetail::Exact;
        if self.tracer.settings.exact != want {
            let mut s = self.tracer.settings.clone();
            s.exact = want;
            self.tracer.set_settings(s);
            self.restart(pane, "traced detail");
        }
        let now = cx.time();
        let publication_changed = self.lights != Some(product.live.lights);
        self.note_gain_paint(cx.cx);
        self.stale = product.stale();
        let camera = state.view.camera(pane);
        let trace_camera = TraceCamera {
            pos: camera.eye, target: camera.target, up: camera.up,
            fov_y: camera.fov_y_deg.to_radians(), f_stop: 0.0,
            focus_dist: camera.distance(),
            ortho_height: camera.ortho.then_some(camera.ortho_height.max(0.001)),
            ..Default::default()
        };
        let sun = if state.view.lighting_for(&state.ui).is_studio() {
            Sun { dir: super::pack::from_render(super::stage::studio_key_dir(camera)), ..Default::default() }
        } else {
            Sun { dir: state.sun.direction(), turbidity: state.sun.turbidity, ..Default::default() }
        };
        let size = ((rect.size.x * cx.current_dpi_factor()) as usize, (rect.size.y * cx.current_dpi_factor()) as usize);
        let size = (size.0.max(4), size.1.max(4));
        let uploaded = !self.stale && product.scene.as_ref().is_some_and(|scene| self.loaded != Some(scene.hash));
        if uploaded {
            let scene = product.scene.as_ref().unwrap();
            self.tracer.set_packed(cx.cx, scene.packed.clone(), self.loaded.is_some());
            self.loaded = Some(scene.hash);
        }
        let white = product.white_to_trace;
        let white_changed = self.gain_white != Some(white.to_bits());
        let mut gain_upload = false;
        if !self.stale && product.scene.as_ref().is_none_or(|scene| scene.groups.is_empty()) {
            self.lights = Some(product.live.lights);
        }
        if let Some(scene) = product.scene.as_ref().filter(|_| !self.stale) {
            if uploaded { self.sent_gains = vec![0.0; scene.groups.len()]; }
            if uploaded || publication_changed || white_changed {
                let mut changes = Vec::new();
                for (index, &comp) in scene.groups.iter().enumerate() {
                    // Half an 8-bit level step is the deadband, but a group going dark is always
                    // sent: a held residual would leave an LED that is off faintly lit.
                    let g = group_gain(comp, &product.levels, white);
                    let last = self.sent_gains[index];
                    if uploaded || white_changed || (g != last && (g == 0.0 || (g - last).abs() >= (0.5 / 255.0) * white)) {
                        changes.push((index as u32 + 1, [g; 3]));
                        self.sent_gains[index] = g;
                    }
                }
                let before = self.tracer.stats.gain_uploads;
                self.tracer.set_light_group_gains(cx.cx, &changes);
                gain_upload = self.tracer.stats.gain_uploads != before;
                if gain_upload {
                    let arrival = self.observed_lights.filter(|(id, _)| *id == product.live.lights)
                        .map_or_else(frame_trace::presented_frames, |(_, frame)| frame);
                    // Publications can overtake a recording waiting for its paint.
                    // Retain only recorded uploads and the latest pending upload.
                    let recorded = self.tracer.recorded_gain_upload();
                    self.gain_frames.retain(|(upload, _)| *upload <= recorded);
                    self.gain_frames.push_back((self.tracer.stats.gain_uploads, arrival));
                }
                self.gain_white = Some(white.to_bits());
                self.lights = Some(product.live.lights);
            }
        }
        let camera_changed = self.schedule.set_camera(trace_camera.clone());
        let environment_key = environment.as_ref().map(|(light, frame)| (Arc::as_ptr(light) as usize, *frame));
        let light_changed = self.sun.as_ref() != Some(&sun) || self.environment != environment_key;
        let resized = self.size != size;
        if uploaded || camera_changed || light_changed || resized {
            self.tracer.set_camera(trace_camera.clone());
            self.tracer.set_sun(&sun, vec3(0.0, 0.0, 1.0));
            self.tracer.set_environment(cx.cx, environment);
            self.tracer.set_size(size.0, size.1);
            self.sun = Some(sun);
            self.environment = environment_key;
            self.size = size;
            if !camera_changed { self.schedule.restart(); }
            let why = if uploaded { "scene/material upload" } else if camera_changed { "camera" } else if light_changed { "lighting" } else { "size" };
            log!("fab pane {} trace_restarts={} ({why})", pane.0, self.schedule.trace_restarts);
        }
        let interacting = state.view.panes.iter().flatten().any(|pane| pane.interacting);
        let frame = cx.drawing_id();
        let fresh_frame = self.last_draw != Some(frame);
        let advance = self.schedule.advance(self.loaded.is_some(), self.stale, interacting);
        let gain_pending = !self.gain_frames.is_empty() && !self.stale;
        let draw = fresh_frame && (advance || gain_pending);
        self.last_draw = Some(frame);
        if draw {
            // While the person acts a trace step stays within the frame's slack, so the frames
            // they watch come on time. Otherwise, a running bench beside the shot included (it
            // waits on no input), steps grow to `IDLE_STEP_INTERVALS`, the wait the first acted
            // frame may meet, and the shot converges fastest; a light's change is presented by
            // the next step, whatever its size.
            let acted = std::mem::take(&mut self.acted);
            let playing = cx.get_global_ref::<ElecLive>().is_some_and(|live| live.sim.status.playing);
            self.tracer.settings.interactive = acted;
            self.tracer.settings.frame_budget = IDLE_STEP_INTERVALS * cx.display_frame_interval();
            if (acted || playing) && advance { self.busy_frames += 1; }
            self.recorded_at = Some(cx.repaint_id());
            self.tracer.set_view_projection(Some(camera.view_projection((rect.size.x / rect.size.y) as f32)));
            cx.make_child_pass(&self.anchor);
            self.anchor.set_size(cx, dvec2(1.0, 1.0));
            self.anchor.set_color_texture(cx, &self.anchor_texture, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
            cx.begin_pass(&self.anchor, Some(1.0));
            self.anchor_list.begin_always(cx);
            cx.begin_root_turtle(dvec2(1.0, 1.0), Layout::flow_overlay());
            self.tracer.set_parent_pass(Some(self.anchor.draw_pass_id()));
            self.tracer.set_skip_trace(!advance);
            if advance {
                self.tracer.draw_scheduled_sample(cx);
            } else {
                self.tracer.draw(cx);
            }
            cx.end_pass_sized_turtle();
            self.anchor_list.end(cx);
            cx.end_pass(&self.anchor);
            self.schedule.progress(self.tracer.stats.spp, self.tracer.stats.done);
            if advance {
                self.traced_frames += 1;
                self.log_rate(pane, now);
            }
        } else if fresh_frame {
            self.detach(cx.cx);
        }
        let progress = TraceProgress {
            samples: self.schedule.samples, target: self.schedule.target, stale: self.stale,
            fields_not_traced: product.fields_not_traced || product.scene.as_ref().is_some_and(|p| p.fields_not_traced),
            bodies_not_traced: product.scene.as_ref().is_some_and(|p| p.bodies_not_traced),
            unavailable: product.failed,
        };
        let shown = &mut state.view.pane_mut(pane).trace;
        if *shown != progress {
            if now - self.chip_at >= 0.25 {
                *shown = progress;
                self.chip_at = now;
                if let Some(timer) = self.chip_timer.take() { cx.stop_timer(timer); }
                crate::ui::redraw(cx.cx, &[ids!(pane_label)]);
            } else if self.chip_timer.is_none() {
                self.chip_timer = Some(cx.start_timeout((0.25 - (now - self.chip_at)).max(0.001)));
            }
        }
        let from = *self.gain_log_at.get_or_insert(now);
        if now - from >= 1.0 {
            let uploads = self.tracer.stats.gain_uploads - self.logged_uploads;
            if uploads != 0 {
                log!("fab pane {} light gains: uploads={:.1}/s groups={} max_latency_frames={}", pane.0,
                    uploads as f64 / (now - from), self.tracer.stats.light_groups, self.max_latency_frames);
            }
            self.gain_log_at = Some(now);
            self.logged_uploads = self.tracer.stats.gain_uploads;
            self.max_latency_frames = 0;
        }
        gain_upload || gain_pending || self.schedule.wants_frame(self.loaded.is_some(), self.stale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::state::POSE_IDENTITY;

    #[test]
    fn light_publications_update_gains_and_presses_request_pool_packing() {
        use fab_elec_core::{BoardId, CompId};
        let comp = CompRef { board: BoardId(0), comp: CompId(0) };
        let rest = LivePublication { lights: Some(1), held: Some(Arc::new(Vec::new())), knobs: None };
        assert_eq!(rest.changes_since(&rest), LiveChanges { gains: false, pack: false },
            "unchanged publication causes neither upload nor pool job");
        let lit = LivePublication { lights: Some(2), ..rest.clone() };
        assert_eq!(lit.changes_since(&rest), LiveChanges { gains: true, pack: false },
            "a light publication selects a gain update without a pack");
        let press = LivePublication { held: Some(Arc::new(vec![comp])), ..lit.clone() };
        assert_eq!(press.changes_since(&lit), LiveChanges { gains: false, pack: true },
            "a held-set publication selects the scene-building pool job");
        let both = LivePublication { lights: Some(3), ..press.clone() };
        assert_eq!(both.changes_since(&lit), LiveChanges { gains: true, pack: true },
            "simultaneous press and light update retain both decisions");
        let release = LivePublication { held: Some(Arc::new(Vec::new())), ..both.clone() };
        assert!(release.changes_since(&both).pack, "releasing an actuator also repacks on the pool");
        let levels = HashMap::from([(comp, 0.25)]);
        assert_eq!(group_gain(comp, &levels, 4.0), 1.0,
            "gain converts row y into scene radiance");
        assert_eq!(group_gain(comp, &HashMap::new(), 4.0), 0.0,
            "an unresolved live row keeps the reserved emitter dark");
    }

    #[test]
    fn prepared_electronics_emits_through_a_lens_only() {
        use super::super::bodies::tests::led_scene;
        use fab_elec_core::mm;

        let (mut scene, _) = led_scene(true);
        Arc::make_mut(&mut scene).boards[0].bodies.reverse();
        let prepared = PreparedElectronics::build(&scene, false).expect("LED scene prepares");
        assert!(prepared.emitting_lens, "an LED using a cached lens mesh marks the product as emitting");

        let (scene, _) = led_scene(false);
        let prepared = PreparedElectronics::build(&scene, false).expect("LED scene without a lens prepares");
        assert!(!prepared.emitting_lens, "an emitter without a lens material does not mark the product as emitting");

        let mut generated = (*scene).clone();
        for body in &mut generated.boards[0].bodies {
            body.mesh = MeshRef::Box { min: [0; 3], max: [mm(1.0); 3] };
        }
        let prepared = PreparedElectronics::build(&Arc::new(generated), false).expect("generated bodies prepare");
        assert!(prepared.emitting_lens, "a generated emitter's top lens marks the product as emitting, as in the raster layer");
        assert_eq!(prepared.bodies[0][0].iter().filter(|prim| prim.lens).count(), 1, "a generated emitter has one lens primitive");
        assert!(prepared.bodies[0][1].iter().all(|prim| !prim.lens), "a generated non-emitter has no lens");
    }

    #[test]
    fn tracing_stops_restarts_and_yields_without_building_for_a_camera() {
        let mut schedule = TraceSchedule::default();
        let mut state = AppState::default();
        let input_key = ProductInputs::key(&state, None, None);
        let mut camera = TraceCamera { pos: state.view.camera(PaneId::PRIMARY).eye, ..Default::default() };
        assert!(schedule.set_camera(camera.clone()));
        schedule.progress(256.0, true);
        assert!(!schedule.wants_frame(true, false), "converged panes request no frame");
        assert!(!schedule.set_camera(camera.clone()), "an unchanged camera does not restart");
        state.view.camera_mut(PaneId::PRIMARY).eye.x += 0.1;
        camera.pos = state.view.camera(PaneId::PRIMARY).eye;
        assert!(schedule.set_camera(camera));
        assert_eq!(schedule.trace_restarts, 2, "camera restarts accumulation once after initial setup");
        assert_eq!(ProductInputs::key(&state, None, None), input_key, "camera change cannot submit a product build");
        assert!(schedule.wants_frame(true, false), "camera change resumes sampling");
        assert!(!schedule.advance(true, false, true), "any interaction suppresses GPU work");
        assert!(!schedule.wants_frame(true, true), "stale pictures await the pool signal");
        assert!(!schedule.wants_frame(false, false), "a missing scene does not spin for completion");
        assert!(schedule.advance(true, false, false), "rest resumes the full budget");
        // At rest every frame traces until the target, however long that takes: a board's close-up
        // gains about 3.5 samples a second at 60 Hz (here a sixteenth a frame), so 256 take over a
        // minute. Then no frame traces.
        let per_frame = 1.0 / 16.0;
        let mut traced = 0u32;
        while schedule.advance(true, false, false) {
            traced += 1;
            schedule.progress(traced as f32 * per_frame, false);
            assert!(traced <= 60 * 600, "a resting pane converges");
        }
        assert_eq!(schedule.samples, schedule.target, "a resting pane reaches its target sample count");
        assert_eq!(traced, (schedule.target as f32 / per_frame).ceil() as u32, "every frame traced on the way, none after");
        assert!(!schedule.wants_frame(true, false), "a converged pane asks for no frame");
        // a Traced view: a Plate view of its own, Synced to Plate and traced, as its instance keeps it
        let source = state.view.active_plate();
        let view = state.lease_pane(crate::api::PaneRole::Plate);
        let traced = view.id();
        state.show_plate_view(traced, PaneLink::Linked { how: LinkHow::Synced, to: source }, true);
        assert_eq!(state.view.pane(traced).mode, PaneMode::Traced, "Traced view opens beside the primary");
        assert_eq!(state.view.pane(PaneId::PRIMARY).mode, PaneMode::Raster);
        assert_eq!(state.view.pane(traced).link, PaneLink::Linked { how: LinkHow::Synced, to: source });
        let through = PaneLink::Linked { how: LinkHow::Through, to: source };
        state.apply_core(&ShellAction::SetPaneLink { pane: traced, link: through });
        state.apply_core(&ShellAction::SetPaneMode { pane: traced, mode: PaneMode::Raster });
        state.apply_core(&ShellAction::SetPaneMode { pane: traced, mode: PaneMode::Traced });
        assert_eq!(state.view.pane(traced).link, through, "mode changes preserve camera links");
    }

    #[test]
    fn mesh_and_glass_product_have_stable_geometry_and_materials() {
        use crate::model::{Element, ElementRange};
        let mut state = AppState::default();
        assert!(state.elec_scene.is_none(), "pose keys need no boards");
        let board_poses = [Mat4f::identity(), Mat4f::identity()];
        let input_key = ProductInputs::key(&state, Some(&board_poses), None);
        assert_eq!(ProductInputs::key(&state, Some(&board_poses), None), input_key, "unchanged board poses keep the key");
        assert_ne!(ProductInputs::key(&state, None, None), input_key, "missing board poses have a distinct key");
        assert_ne!(ProductInputs::key(&state, Some(&board_poses[..1]), None), input_key, "a board fewer changes the key");
        for board in 0..2 {
            for i in 0..16 {
                let mut moved = board_poses;
                moved[board].v[i] += 1.0;
                assert_ne!(ProductInputs::key(&state, Some(&moved), None), input_key, "board {board}'s pose component {i} changes the key");
            }
        }
        let mut scene = Scene::empty();
        for i in 0..2 {
            let element = ElementId(i);
            let mut batch = RenderBatch { material: MaterialId(i), ..Default::default() };
            for p in [[0.0, 0.0, 0.0], [0.01, 0.0, 0.0], [0.0, 0.01, 0.0]] {
                batch.push_vertex(&Vertex { position: [p[0], p[1], i as f32 * 0.003], normal: [0.0, 0.0, 1.0], element: i as f32, ..Default::default() });
            }
            batch.indices = vec![0, 1, 2];
            batch.element_ranges.push(ElementRange { element, part: i, first_index: 0, index_count: 3, draw_priority: 0, coplanar_group: 0 });
            scene.elements.push(Element { id: element, name: format!("part{i}"), bounds: aabb_empty(), ranges: vec![(i, 0, 3)], triangle_count: 1 });
            scene.materials.push(crate::model::Material {
                id: MaterialId(i), name: String::new(), base_color: [0.3, 0.5, 0.7, 1.0],
                metallic: 0.0, roughness: if i == 1 { 0.0 } else { FILAMENT_ROUGHNESS }, emissive: [0.0; 3],
                ior: 1.5, transmission: i as f32, double_sided: i == 1,
                transparent: i == 1, texture: None,
            });
            scene.batches.push(batch);
            state.plate.parts.push(PlatePart { printed: true, element: Some(element), color: [0.3, 0.5, 0.7, 1.0], ..Default::default() });
            state.glass_parts.elements.insert(element, (i as usize, (i == 1).then_some(i)));
        }
        state.scene = Arc::new(scene);
        assert!(state.is_glass_under(state.shared_poses(), ElementId(1)), "fixture has a real glass part");
        let first = ProductScene::build(ProductInputs::capture(&state, None, None), None).unwrap();
        let second = ProductScene::build(ProductInputs::capture(&state, None, None), Some(&first)).unwrap();
        assert_eq!(first.input.tri_count(), 2, "one triangle per part");
        assert_eq!(first.input.materials.len(), 2, "one filament and one glass material");
        assert_eq!(first.input.materials[0].roughness, FILAMENT_ROUGHNESS);
        assert_eq!(first.input.materials[1].transmission, 1.0);
        assert!(first.input.materials[1].two_sided);
        assert_eq!(first.input.up, vec3(0.0, 0.0, 1.0));
        assert_eq!(first.hash, second.hash, "identical content has a stable hash");
        assert!(!second.geometry_rebuilt, "unchanged geometry is retained");
        state.plate.parts[0].color[0] = 0.8;
        let recolored = ProductScene::build(ProductInputs::capture(&state, None, None), Some(&second)).unwrap();
        assert_eq!(recolored.geometry_hash, second.geometry_hash, "material changes retain geometry");
        assert_ne!(recolored.hash, second.hash, "material changes invalidate accumulation");
        assert!(Arc::ptr_eq(&recolored.blocks[0], &second.blocks[0]), "material changes reuse triangle blocks");
        state.view.camera_mut(PaneId::PRIMARY).eye.x += 0.1;
        let moved_camera = ProductScene::build(ProductInputs::capture(&state, None, None), Some(&recolored)).unwrap();
        assert_eq!(moved_camera.hash, recolored.hash, "the camera is not product content");
        let mut pose = POSE_IDENTITY;
        pose[0][3] = 0.02;
        state.scene_state.set_nudge(Some((ElementId(0), pose)));
        let moved_part = ProductScene::build(ProductInputs::capture(&state, None, None), Some(&moved_camera)).unwrap();
        assert_ne!(moved_part.geometry_hash, moved_camera.geometry_hash, "part poses change geometry");
        assert_eq!(moved_part.input.positions[0][0], 0.02, "poses stay in Z-up metres");
        assert!(Arc::ptr_eq(&moved_part.blocks[1], &moved_camera.blocks[1]), "moving one part keeps the other block");
        // A case the Glass view shows through (an insert's host) in a dark filament traces as
        // the raster draws it: its share of the view through, untinted by the filament.
        state.plate.parts[0].color = [0.02, 0.02, 0.03, 1.0];
        state.glass_parts.hosts.insert(ElementId(0));
        let case = ProductScene::build(ProductInputs::capture(&state, None, None), Some(&moved_part)).unwrap();
        let case = &case.input.materials[0];
        assert_eq!((case.transmission, case.albedo, case.roughness), (super::super::glass::CASE_SEE_THROUGH, [1.0; 3], 0.0),
            "a see-through case passes the raster's share of the view, untinted by its filament");
        assert!(case.two_sided && !case.solid, "a see-through case is a two-sided sheet");
    }
}

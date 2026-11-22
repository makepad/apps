//! Fab's full electronics meshes in the renderer's sun cascades. Geometry and world poses
//! arrive with uploads and edits; live rows and lit uniforms never invalidate these shadows.

use super::*;
use makepad_render::gpu_lightmap::{CascadeCasters, CascadeTile};
use makepad_render::shadow_csm::{cascade_holds_shadow, cascade_overlaps, cascade_sees, cascade_shadow_in_view, CsmCascade};
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

script_mod! {
    use mod.prelude.fab.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    // The same depth and tile mapping as SdfTileShadow. Section planes are in the render
    // world, and clip fragments so a triangle crossing a section still casts its kept side.
    mod.draw.FabMeshShadow = {
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        alpha_blend: false
        depth_write: true
        backface_culling: false
        color_format: @Rf32
        sun_rx: uniform(vec4(1.0, 0.0, 0.0, 0.0))
        sun_ry: uniform(vec4(0.0, 1.0, 0.0, 0.0))
        sun_rz: uniform(vec4(0.0, 0.0, 1.0, 0.0))
        tile_at: uniform(vec4(1.0, 1.0, 0.0, 0.0))
        depth_at: uniform(vec2(1.0, 0.0))
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_world: varying(vec3f)
        v_d: varying(f32)

        project_shadow: fn(w: vec4) -> vec4 {
            self.v_world = w.xyz
            let nx = dot(self.sun_rx.xyz, w.xyz) + self.sun_rx.w
            let ny = dot(self.sun_ry.xyz, w.xyz) + self.sun_ry.w
            let nz = dot(self.sun_rz.xyz, w.xyz) + self.sun_rz.w
            self.v_d = nz
            return vec4(nx * self.tile_at.x + self.tile_at.z, ny * self.tile_at.y + self.tile_at.w, nz * self.depth_at.x + self.depth_at.y, 1.0)
        }

        clipped: fn() -> bool {
            let p = self.v_world
            let c = self.clip_ctl.x
            if c > 0.5 && dot(self.clip0.xyz, p) + self.clip0.w < 0.0 { return true }
            if c > 1.5 && dot(self.clip1.xyz, p) + self.clip1.w < 0.0 { return true }
            if c > 2.5 && dot(self.clip2.xyz, p) + self.clip2.w < 0.0 { return true }
            if c > 3.5 && dot(self.clip3.xyz, p) + self.clip3.w < 0.0 { return true }
            if c > 4.5 && dot(self.clip4.xyz, p) + self.clip4.w < 0.0 { return true }
            if c > 5.5 && dot(self.clip5.xyz, p) + self.clip5.w < 0.0 { return true }
            return false
        }

        fragment: fn() {
            if self.clipped() { discard() }
            self.fb0 = vec4(self.v_d, 0.0, 0.0, 1.0)
        }
    }

    mod.draw.FabBodyShadow = mod.std.set_type_default() do #(DrawFabBodyShadow::script_shader(vm)){
        ..mod.draw.FabMeshShadow,
        geom: vertex_buffer(mod.fab_body_geom.BodyVertex, mod.fab_body_geom.BodyGeom)

        vertex: fn() {
            let w = self.m0 * self.geom.pos.x + self.m1 * self.geom.pos.y + self.m2 * self.geom.pos.z + self.m3
            self.vertex_pos = self.project_shadow(w)
        }
    }

    mod.draw.FabBoardShadow = mod.std.set_type_default() do #(DrawFabBoardShadow::script_shader(vm)){
        ..mod.draw.FabMeshShadow,
        geom: vertex_buffer(mod.fab_board_geom.BoardVertex, mod.fab_board_geom.BoardGeom)
        world_from_obj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))

        vertex: fn() {
            self.vertex_pos = self.project_shadow(self.world_from_obj * vec4(self.geom.pos, self.vertex_w))
        }
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFabBodyShadow {
    #[deref]
    pub draw_vars: DrawVars,
    #[live]
    pub m0: Vec4f,
    #[live]
    pub m1: Vec4f,
    #[live]
    pub m2: Vec4f,
    #[live]
    pub m3: Vec4f,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFabBoardShadow {
    #[deref]
    pub draw_vars: DrawVars,
    /// The draw API needs one instance slot even for a uniform board pose.
    #[live(1.0)]
    pub vertex_w: f32,
}

/// Plate millimetres into the renderer's Y-up metres.
fn world_from_plate_mm() -> Mat4f {
    Mat4f::mul(&render_from_fab(), &Mat4f::scale(0.001))
}

fn world_pose(fab_from_obj: &Mat4f) -> Mat4f {
    Mat4f::mul(&world_from_plate_mm(), fab_from_obj)
}

fn world_box(bounds: &Aabb, pose: &Mat4f) -> Aabb {
    let mut world = aabb_empty();
    if aabb_is_empty(bounds) { return world; }
    for x in [bounds.min.x, bounds.max.x] {
        for y in [bounds.min.y, bounds.max.y] {
            for z in [bounds.min.z, bounds.max.z] {
                let p = pose.transform_vec4(vec4(x, y, z, 1.0));
                world = aabb_union(&world, &Aabb { min: vec3(p.x, p.y, p.z), max: vec3(p.x, p.y, p.z) });
            }
        }
    }
    world
}

struct BodyCaster {
    board: usize,
    pose: Mat4f,
    bounds: Aabb,
}

pub(super) struct BodyCasters {
    geometry: Rc<Geometry>,
    local_bounds: Aabb,
    /// Non-casting slots retain the lit batch's indices without retaining a pose or box.
    instances: Vec<Option<BodyCaster>>,
    /// Distinct boards with casting instances in this batch.
    boards: Vec<usize>,
    /// Tight union after upload or a full re-pose. A live move invalidates it in O(1);
    /// the next cascade draw refits it once, shared by all tiles until another pose change.
    bounds: Option<Aabb>,
}

impl BodyCasters {
    pub(super) fn new(geometry: Rc<Geometry>, bounds: Aabb, instances: impl Iterator<Item = (f32, usize, Mat4f)>) -> Self {
        let mut boards = Vec::new();
        let instances = instances.map(|(translucency, board, fab_from_body)| {
            // Any transmission passes a shadow ray, including a supplier mesh's lens.
            if translucency > 0.0 { return None; }
            if let Err(index) = boards.binary_search(&board) { boards.insert(index, board); }
            let pose = world_pose(&fab_from_body);
            Some(BodyCaster { board, pose, bounds: world_box(&bounds, &pose) })
        }).collect();
        let mut batch = Self { geometry, local_bounds: bounds, instances, boards, bounds: None };
        batch.bounds = Some(batch.world_bounds());
        batch
    }

    fn world_bounds(&self) -> Aabb {
        self.instances.iter().flatten().fold(aabb_empty(), |bounds, instance| aabb_union(&bounds, &instance.bounds))
    }
}

pub(super) struct BoardCaster {
    edges: Rc<Geometry>,
    faces: Option<Rc<Geometry>>,
    local_bounds: Aabb,
    pose: Mat4f,
    bounds: Aabb,
}

impl BoardCaster {
    pub(super) fn new(edges: Rc<Geometry>, faces: Option<Rc<Geometry>>, bounds: Aabb, fab_from_board: &Mat4f) -> Self {
        let pose = world_pose(fab_from_board);
        Self { edges, faces, local_bounds: bounds, pose, bounds: world_box(&bounds, &pose) }
    }
}

#[derive(Default)]
pub(super) struct ElecCasters {
    bodies: Vec<BodyCasters>,
    boards: Vec<BoardCaster>,
    content: u64,
    moves_hash: u64,
    world_moves: Vec<Option<Mat4f>>,
    has_moves: bool,
    planes: [[f32; 4]; 6],
    plane_count: usize,
    body_draw: Option<DrawFabBodyShadow>,
    board_draw: Option<DrawFabBoardShadow>,
    selection: CasterSelection,
    /// The boards' placements keep them out of the views (`BoardLayer::hidden`): nothing casts.
    hidden: bool,
}

impl ElecCasters {
    pub(super) fn set_hidden(&mut self, hidden: bool) {
        if self.hidden == hidden { return; }
        self.hidden = hidden;
        self.content = self.content.wrapping_add(1);
    }

    /// Called at body upload and board creation, before the renderer can open a cascade pass.
    pub(super) fn ensure_shaders(&mut self, cx: &mut Cx) {
        if self.body_draw.is_none() {
            self.body_draw = cx.try_with_vm(|vm| DrawFabBodyShadow::script_new_with_default(vm));
        }
        if self.board_draw.is_none() {
            self.board_draw = cx.try_with_vm(|vm| DrawFabBoardShadow::script_new_with_default(vm));
        }
    }

    pub(super) fn revision(&self) -> u64 {
        let mut hash = DefaultHasher::new();
        (self.content, self.moves_hash).hash(&mut hash);
        hash.finish()
    }

    #[cfg(test)]
    pub(super) fn content(&self) -> u64 { self.content }

    /// Each pane's cascade cache can recognize its own moves again after another pane draws.
    pub(super) fn set_moves(&mut self, moves: &[Option<Mat4f>]) {
        let moves_hash = board::moves_hash(moves);
        if moves.is_empty() {
            self.world_moves.clear();
            self.has_moves = false;
            self.moves_hash = moves_hash;
            return;
        }
        if self.moves_hash == moves_hash && self.world_moves.len() == moves.len() { return; }
        let world_from_plate = world_from_plate_mm();
        let plate_from_world = world_from_plate.invert();
        self.world_moves.clear();
        self.world_moves.extend(moves.iter().map(|movement| {
            movement.as_ref().map(|m| Mat4f::mul(&Mat4f::mul(&world_from_plate, m), &plate_from_world))
        }));
        self.has_moves = moves.iter().any(Option::is_some);
        self.moves_hash = moves_hash;
    }

    fn world_move(&self, board: usize) -> Option<&Mat4f> {
        self.world_moves.get(board)?.as_ref()
    }

    pub(super) fn set_bodies(&mut self, bodies: Vec<BodyCasters>) {
        self.content = self.content.wrapping_add(1);
        self.bodies = bodies;
    }

    /// The caller has compared the lit pose, including a non-casting body's pose. Only a
    /// moved body's eight corners are transformed; no batch scan accompanies a live drag.
    pub(super) fn set_body_pose(&mut self, batch: usize, instance: usize, fab_from_body: &Mat4f) {
        self.content = self.content.wrapping_add(1);
        let batch = &mut self.bodies[batch];
        if let Some(instance) = &mut batch.instances[instance] {
            instance.pose = world_pose(fab_from_body);
            instance.bounds = world_box(&batch.local_bounds, &instance.pose);
            batch.bounds = None;
        }
    }

    /// A full re-pose tightens its batches in O(bodies). Live moves defer this until a cascade
    /// is drawn, once per changed batch; the next tiles reuse the exact union without a scan.
    pub(super) fn refit_bodies(&mut self) {
        for batch in &mut self.bodies {
            if batch.bounds.is_none() { batch.bounds = Some(batch.world_bounds()); }
        }
    }

    pub(super) fn set_boards(&mut self, boards: Vec<BoardCaster>) {
        if self.boards.is_empty() && boards.is_empty() { return; }
        self.boards = boards;
        self.content = self.content.wrapping_add(1);
    }

    pub(super) fn set_board_pose(&mut self, index: usize, fab_from_board: &Mat4f) {
        let board = &mut self.boards[index];
        let pose = world_pose(fab_from_board);
        if board.pose.v == pose.v { return; }
        board.pose = pose;
        board.bounds = world_box(&board.local_bounds, &pose);
        self.content = self.content.wrapping_add(1);
    }

    pub(super) fn set_clip_planes(&mut self, planes: &[[f32; 4]]) {
        let count = planes.len().min(6);
        if self.plane_count == count && self.planes[..count] == planes[..count] { return; }
        self.planes = [[0.0; 4]; 6];
        self.planes[..count].copy_from_slice(&planes[..count]);
        self.plane_count = count;
        self.content = self.content.wrapping_add(1);
    }

    fn draw_cascade(&mut self, cx: &mut CxDraw, at: &CascadeTile) -> usize {
        self.refit_bodies();
        let mut selected = std::mem::take(&mut self.selection);
        select_casters(at.cascade, at.finer, self, &mut selected);
        let mut draws = 0;
        if let Some(draw) = &mut self.body_draw {
            bind_tile(cx.cx, &mut draw.draw_vars, at, &self.planes[..self.plane_count]);
            for (batch, selected) in self.bodies.iter().zip(&selected.bodies) {
                if selected.indices.is_empty() || !draw.draw_vars.can_instance() { continue; }
                draw.draw_vars.geometry_id = Some(batch.geometry.geometry_id());
                if cx.new_draw_call(&mut draw.draw_vars).is_none() { continue; }
                for (index, moved) in selected.iter() {
                    let m = &moved.unwrap_or_else(|| &batch.instances[index].as_ref().unwrap().pose).v;
                    draw.m0 = vec4(m[0], m[1], m[2], m[3]);
                    draw.m1 = vec4(m[4], m[5], m[6], m[7]);
                    draw.m2 = vec4(m[8], m[9], m[10], m[11]);
                    draw.m3 = vec4(m[12], m[13], m[14], m[15]);
                    let area = cx.add_instance(&mut draw.draw_vars);
                    draw.draw_vars.area = cx.update_area_refs(draw.draw_vars.area, area);
                }
                draws += 1;
            }
        }
        if let Some(draw) = &mut self.board_draw {
            bind_tile(cx.cx, &mut draw.draw_vars, at, &self.planes[..self.plane_count]);
            draw.vertex_w = 1.0;
            for (index, moved) in selected.boards.iter() {
                let board = &self.boards[index];
                draw.draw_vars.set_uniform(cx.cx, id!(world_from_obj), &moved.unwrap_or(&board.pose).v);
                for geometry in std::iter::once(&board.edges).chain(board.faces.iter()) {
                    draw.draw_vars.geometry_id = Some(geometry.geometry_id());
                    if !draw.draw_vars.can_instance() || cx.new_draw_call(&mut draw.draw_vars).is_none() { continue; }
                    let area = cx.add_instance(&mut draw.draw_vars);
                    draw.draw_vars.area = cx.update_area_refs(draw.draw_vars.area, area);
                    draws += 1;
                }
            }
        }
        self.selection = selected;
        draws
    }
}

fn bind_tile(cx: &Cx, vars: &mut DrawVars, at: &CascadeTile, planes: &[[f32; 4]]) {
    let row = |r: Vec4f| [r.x, r.y, r.z, r.w];
    vars.set_uniform(cx, id!(sun_rx), &row(at.cascade.rx));
    vars.set_uniform(cx, id!(sun_ry), &row(at.cascade.ry));
    vars.set_uniform(cx, id!(sun_rz), &row(at.cascade.rz));
    vars.set_uniform(cx, id!(tile_at), &row(at.tile));
    vars.set_uniform(cx, id!(depth_at), &[at.depth.0, at.depth.1]);
    vars.set_uniform(cx, id!(clip_ctl), &[planes.len() as f32, 0.0, 0.0, 0.0]);
    for (index, name) in [id!(clip0), id!(clip1), id!(clip2), id!(clip3), id!(clip4), id!(clip5)].into_iter().enumerate() {
        vars.set_uniform(cx, name, planes.get(index).unwrap_or(&[0.0; 4]));
    }
    vars.options.scissor = at.scissor;
}

/// Indices stay in draw order. Keep just the moved poses so drawing can reuse culling's multiply.
#[derive(Default)]
struct SelectedCasters {
    indices: Vec<usize>,
    moved: Vec<(usize, Mat4f)>,
}

impl SelectedCasters {
    /// Reserve on first use so changing which casters a tile sees needs no allocation.
    fn reset(&mut self, count: usize, has_moves: bool) {
        self.indices.clear();
        self.indices.reserve(count);
        self.moved.clear();
        if has_moves { self.moved.reserve(count); }
    }

    fn select(&mut self, index: usize, movement: Option<&Mat4f>, pose: &Mat4f, local_bounds: &Aabb, bounds: &Aabb, keep: &impl Fn(&Aabb) -> bool) {
        if let Some(movement) = movement {
            let pose = Mat4f::mul(movement, pose);
            if !keep(&world_box(local_bounds, &pose)) { return; }
            self.moved.push((index, pose));
        } else if !keep(bounds) {
            return;
        }
        self.indices.push(index);
    }

    fn iter(&self) -> impl Iterator<Item = (usize, Option<&Mat4f>)> {
        let mut moved = self.moved.iter().peekable();
        self.indices.iter().map(move |&index| {
            (index, moved.next_if(|(i, _)| *i == index).map(|(_, pose)| pose))
        })
    }
}

/// Selection needs no GPU and allocates only as it grows.
#[derive(Default)]
pub(super) struct CasterSelection {
    bodies: Vec<SelectedCasters>,
    boards: SelectedCasters,
}

pub(super) fn select_casters(cascade: &CsmCascade, finer: Option<&CsmCascade>, member: &ElecCasters, scratch: &mut CasterSelection) {
    let keep = |bounds: &Aabb| {
        !aabb_is_empty(bounds) && cascade_overlaps(cascade, bounds.min, bounds.max)
            && cascade_sees(cascade, bounds.min, bounds.max) && cascade_shadow_in_view(cascade, bounds.min, bounds.max)
            && !finer.is_some_and(|f| cascade_holds_shadow(f, bounds.min, bounds.max, bounds.min.y.max(0.0)))
    };
    scratch.bodies.resize_with(member.bodies.len(), SelectedCasters::default);
    let has_moves = !member.hidden && member.has_moves;
    scratch.boards.reset(member.boards.len(), has_moves);
    for (batch, selected) in member.bodies.iter().zip(&mut scratch.bodies) {
        let moved = has_moves && batch.boards.iter().any(|&board| member.world_move(board).is_some());
        selected.reset(batch.instances.len(), moved);
        if member.hidden { continue; }
        if moved {
            // A holder can carry a body into a tile entirely outside the batch's rest box.
            for (index, instance) in batch.instances.iter().enumerate() {
                let Some(instance) = instance else { continue };
                selected.select(index, member.world_move(instance.board), &instance.pose, &batch.local_bounds, &instance.bounds, &keep);
            }
        } else {
            // Draws refit dirty batches. Pure callers can also select directly after an edit.
            let bounds = batch.bounds.unwrap_or_else(|| batch.world_bounds());
            if !keep(&bounds) { continue; }
            for (index, instance) in batch.instances.iter().enumerate() {
                if instance.as_ref().is_some_and(|instance| keep(&instance.bounds)) { selected.indices.push(index); }
            }
        }
    }
    for (index, board) in member.boards.iter().enumerate() {
        if !member.hidden {
            scratch.boards.select(index, member.world_move(index), &board.pose, &board.local_bounds, &board.bounds, &keep);
        }
    }
}

/// Fab owns the two caster sets; the renderer sees one revision and one draw hook.
pub(super) struct SceneCasters {
    sdf: Rc<RefCell<sdf::SdfLayer>>,
    elec: Rc<RefCell<ElecCasters>>,
}

impl SceneCasters {
    pub(super) fn new(sdf: Rc<RefCell<sdf::SdfLayer>>, elec: Rc<RefCell<ElecCasters>>) -> Self { Self { sdf, elec } }
}

impl CascadeCasters for SceneCasters {
    fn revision(&self) -> u64 {
        let mut hash = DefaultHasher::new();
        (self.sdf.borrow().revision(), self.elec.borrow().revision()).hash(&mut hash);
        hash.finish()
    }

    fn draw_cascade(&mut self, cx: &mut CxDraw, at: &CascadeTile) -> usize {
        self.sdf.borrow_mut().draw_cascade(cx, at) + self.elec.borrow_mut().draw_cascade(cx, at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_render::shadow_csm::{fit_cascades, CsmView};

    #[test]
    fn electronics_select_shadow_reach_and_leave_held_shadows_to_the_finer_tile() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let geometry = Rc::new(Geometry::new(&mut cx));
        // A top view shows the middle 20 mm of an 80 x 50 mm board. The key light puts an
        // off-screen body's shadow on that middle; a second body sits in the finest tile.
        let view = CsmView {
            cam: vec3(0.0, 0.1, 0.0),
            near_corners: [vec3(-0.01, 0.099, -0.01), vec3(0.01, 0.099, -0.01), vec3(-0.01, 0.099, 0.01), vec3(0.01, 0.099, 0.01)],
            far_corners: [vec3(-0.01, -0.1, -0.01), vec3(0.01, -0.1, -0.01), vec3(-0.01, -0.1, 0.01), vec3(0.01, -0.1, 0.01)],
            focus_distance: 0.1,
        };
        let sun = vec3(0.8, 0.6, 0.0);
        let frame = fit_cascades(Some(&view), view.cam, sun, vec3(-0.22, 0.0, -0.03), vec3(0.04, 0.02, 0.22), 0.2, 2048.0);
        let instance = |board, at: Vec3f, translucency| (translucency, board, Mat4f::translation(vec3(at.x, -at.z, at.y) * 1000.0));
        let instances = [
            instance(0, vec3(0.016, 0.012, 0.0), 0.0),
            instance(1, vec3(0.0, 0.004, 0.0), 0.0),
            instance(1, vec3(0.0, 0.004, 0.0), f32::EPSILON),
            // Widens the batch so coarse selection must also check each instance's shadow.
            instance(1, vec3(-0.2, 0.004, 0.2), 0.0),
        ];
        let bounds = Aabb { min: vec3(-1.0, -1.0, -1.0), max: vec3(1.0, 1.0, 1.0) };
        let mut member = ElecCasters::default();
        member.set_bodies(vec![
            BodyCasters::new(geometry.clone(), bounds, instances.iter().copied()),
            BodyCasters::new(geometry.clone(), bounds, instances[2..3].iter().copied()),
            // This batch's entire rest box is outside the view.
            BodyCasters::new(geometry.clone(), bounds, instances[3..].iter().copied()),
        ]);
        let slab = Aabb { min: vec3(-40.0, -25.0, 0.0), max: vec3(40.0, 25.0, 1.6) };
        member.set_boards(vec![
            BoardCaster::new(geometry.clone(), Some(geometry.clone()), slab, &Mat4f::identity()),
            BoardCaster::new(geometry.clone(), Some(geometry), slab, &instances[3].2),
        ]);
        let off = &member.bodies[0].instances[0].as_ref().unwrap().bounds;
        assert!(off.min.x > 0.01, "the off-screen body is wholly outside the camera's right edge");
        let hit = (off.min + off.max) * 0.5 - sun * (0.012 / sun.y);
        assert!(hit.x.abs() < 0.01 && hit.z.abs() < 0.01, "the off-screen body's shadow reaches the visible board");
        let near = &member.bodies[0].instances[1].as_ref().unwrap().bounds;
        assert!(cascade_holds_shadow(&frame.cascades[0], near.min, near.max, near.min.y.max(0.0)),
            "the finer cascade holds the near body's entire shadow down to the bed");
        let mut selected = CasterSelection::default();
        select_casters(&frame.cascades[0], None, &member, &mut selected);
        assert!(selected.bodies[0].indices.contains(&0), "an off-screen body casting into view is selected");
        assert!(selected.bodies[0].indices.contains(&1), "the finer cascade selects the shadow it holds");
        assert_eq!(selected.boards.indices, [0], "the slab's whole faces and edges are selected together");
        assert!(selected.bodies[2].indices.is_empty(), "the distant batch casts outside the tile at rest");
        select_casters(&frame.cascades[1], None, &member, &mut selected);
        assert!(selected.bodies[0].indices.contains(&1), "the near body overlaps the coarser cascade before finer coverage is applied");
        select_casters(&frame.cascades[1], Some(&frame.cascades[0]), &member, &mut selected);
        assert!(!selected.bodies[0].indices.contains(&1), "the coarser cascade leaves a wholly held shadow to the finer one");
        for (index, cascade) in frame.cascades.iter().enumerate() {
            select_casters(cascade, None, &member, &mut selected);
            assert!(!selected.bodies[0].indices.contains(&2), "cascade {index}: any positive transmission excludes a body");
            assert!(selected.bodies[1].indices.is_empty(), "cascade {index}: an entirely translucent batch casts nothing");
        }
        member.set_moves(&[None, Some(Mat4f::translation(vec3(200.0, 200.0, 0.0)))]);
        select_casters(&frame.cascades[0], None, &member, &mut selected);
        assert_eq!(selected.bodies[0].indices, [0, 3], "the holder carries the near body out and the outlier in while the other board stays put");
        assert_eq!(selected.bodies[2].indices, [0], "the moved body enters the tile even when its whole batch rests outside");
        assert_eq!(selected.boards.indices, [0, 1], "the moved slab casts alongside the stationary slab");
        member.set_moves(&[None, None]);
        select_casters(&frame.cascades[0], None, &member, &mut selected);
        assert_eq!(selected.bodies[0].indices, [0, 1], "a pane at rest still selects the original bodies");
        assert_eq!(selected.boards.indices, [0], "a pane at rest still selects the original slab");

        // A live move invalidates the tight batch box; the next draw's refit must shrink it too.
        let revision = member.revision();
        member.set_body_pose(0, 3, &instances[1].2);
        assert_ne!(member.revision(), revision, "a live pose edit invalidates the caster set");
        select_casters(&frame.cascades[0], None, &member, &mut selected);
        assert!(selected.bodies[0].indices.contains(&3), "selection sees a moved body even before the draw refits its batch");
        member.refit_bodies();
        let tight = member.bodies[0].bounds.unwrap();
        assert!(tight.min.x > -0.01 && tight.max.z < 0.01, "moving the outlier inward tightens the batch union");
        let capacity = selected.bodies[0].indices.capacity();
        let revision = member.revision();
        select_casters(&frame.cascades[0], None, &member, &mut selected);
        assert_eq!(selected.bodies[0].indices.capacity(), capacity, "another tile reuses grown selection storage");
        assert_eq!(member.revision(), revision, "selection itself never moves the member's revision");
    }
}

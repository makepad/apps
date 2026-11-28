//! Layered surfaces for every board in the electronics host's committed scene.

use super::board_geom::{face_tile, BoardMesh};
use super::board_scene::{art_box, prepare_scene, SceneBoardPrep};
use super::*;
use fab_elec_core::{BoardId, Side};
use fab_elec_mech::scene::ElecScene;
use fab_elec_view::board::art::ArtRaster;
use fab_elec_view::board::tiles::{ArtPlane, BoardArt, LayerArt, TileId, ART_BASE_NM, ART_TILE_TEXELS};
use fab_elec_view::board::BoardDraws;
use std::cell::RefCell;
use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::rc::Rc;

script_mod! {
    use mod.prelude.fab.*
    use mod.math.*
    use mod.shader.*
    use mod.draw

    mod.fab_board_geom = {
        BoardVertex: #(board_vertex_pod(vm))
        BoardGeom: #(board_placeholder_geom(vm))
    }

    // Both passes use exactly the same relief, drill and section predicates.
    mod.draw.FabBoardSurface = {
        alpha_blend: false
        depth_write: true
        cull_mode: @None
        vertex_pos: vertex_position(vec4f)
        fb0: fragment_output(0, vec4f)
        draw_call: uniform_buffer(draw.DrawCallUniforms)
        draw_pass: uniform_buffer(draw.DrawPassUniforms)
        draw_list: uniform_buffer(draw.DrawListUniforms)
        geom: vertex_buffer(mod.fab_board_geom.BoardVertex, mod.fab_board_geom.BoardGeom)
        world_from_obj: uniform(mat4x4f(1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0))
        art_rect: uniform(vec4(0.0, 0.0, 1.0, 0.0))
        stack: uniform(vec4(1.6, 0.035, 0.02, 0.015))
        code: uniform(16000008.0)
        art: texture_2d(float)
        clip_ctl: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip0: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip1: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip2: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip3: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip4: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip5: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        clip_cut: uniform(vec4(0.0, 0.0, 0.0, 0.0))
        v_obj: varying(vec3f)
        v_world: varying(vec3f)
        v_view: varying(vec3f)
        v_eye: varying(vec3f)
        v_normal: varying(vec3f)
        v_uv: varying(vec2f)
        v_kind: varying(float)
        v_plated: varying(float)
        v_t: varying(float)

        vertex: fn() {
            let w = self.world_from_obj * vec4(self.geom.pos, self.vertex_w)
            let v = self.draw_pass.camera_view * w
            self.v_obj = self.geom.pos
            self.v_world = w.xyz
            self.v_view = v.xyz
            self.v_normal = self.geom.normal
            self.v_uv = self.geom.uv
            self.v_kind = self.geom.kind
            self.v_plated = self.geom.plated
            self.v_t = self.geom.t
            self.vertex_pos = self.draw_pass.camera_projection * v
            if self.clip_ctl.z > 0.5 {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 0.0, 1.0)).xyz
            } else {
                self.v_eye = (self.draw_pass.camera_inv * vec4(0.0, 0.0, 1.0, 0.0)).xyz
            }
        }

        art_at: fn(uv: vec2f) -> vec4f {
            // Walls and barrels carry no layer art and never sample the texture.
            if self.v_kind > 1.5 { return vec4(0.0, 0.0, 0.0, 0.0) }
            return self.art.sample(uv)
        }

        height_at: fn(uv: vec2f) -> float {
            let a = self.art_at(uv)
            return self.stack.y * a.x + self.stack.w * a.z - self.stack.z * a.y
        }

        // fwidth in board millimetres, expressed through the supported derivative intrinsics.
        relief_fade: fn() -> float {
            let footprint = abs(dFdx(self.v_uv)) + abs(dFdy(self.v_uv))
            let pixels = (self.art_rect.z / 256.0) / max(max(footprint.x, footprint.y), 0.000001)
            return smoothstep(0.5, 1.0, pixels)
        }

        board_normal: fn(uv: vec2f, a: vec4f, fade: float) -> vec3f {
            if self.v_kind > 1.5 { return normalize(self.v_normal) }
            let step_mm = max(self.art_rect.z / 256.0, 0.000001)
            let dx = vec2(1.0 / 260.0, 0.0)
            let dy = vec2(0.0, 1.0 / 260.0)
            let fine = vec2(self.height_at(uv + dx) - self.height_at(uv - dx),
                self.height_at(uv + dy) - self.height_at(uv - dy)) / (2.0 * step_mm)
            // Twice the difference span, then half the slope under the rounding mask.
            let rounded = vec2(self.height_at(uv + dx * 2.0) - self.height_at(uv - dx * 2.0),
                self.height_at(uv + dy * 2.0) - self.height_at(uv - dy * 2.0)) / (8.0 * step_mm)
            let slope = mix(fine, rounded, 1.0 - a.y) * fade
            var z = 1.0
            if self.v_kind > 0.5 { z = 0.0 - 1.0 }
            return normalize(vec3(0.0 - slope.x, 0.0 - slope.y, z))
        }

        eye_dir: fn() -> vec3f {
            if self.clip_ctl.z > 0.5 { return normalize(self.v_eye - self.v_world) }
            return normalize(self.v_eye)
        }

        surface_normal: fn(uv: vec2f, a: vec4f, fade: float, v: vec3f) -> vec3f {
            var n = normalize((self.world_from_obj * vec4(self.board_normal(uv, a, fade), 0.0)).xyz)
            let away = dot(n, v)
            if away < 0.0 {
                let side = n - v * away
                if dot(side, side) > 0.0001 { n = normalize(side) } else { n = 0.0 - n }
            }
            return n
        }

        cut_margin: fn() -> float {
            if self.clip_ctl.z > 0.5 { return self.clip_ctl.y * length(self.v_view) }
            return self.clip_ctl.y
        }

        clipped: fn(p: vec3f) -> bool {
            let m = 0.0 - self.cut_margin()
            if dot(self.clip_cut.xyz, (self.world_from_obj * vec4(p, 1.0)).xyz) + self.clip_cut.w < m { return true }
            let c = self.clip_ctl.x
            if c > 0.5 && dot(self.clip0.xyz, p) + self.clip0.w < m { return true }
            if c > 1.5 && dot(self.clip1.xyz, p) + self.clip1.w < m { return true }
            if c > 2.5 && dot(self.clip2.xyz, p) + self.clip2.w < m { return true }
            if c > 3.5 && dot(self.clip3.xyz, p) + self.clip3.w < m { return true }
            if c > 4.5 && dot(self.clip4.xyz, p) + self.clip4.w < m { return true }
            if c > 5.5 && dot(self.clip5.xyz, p) + self.clip5.w < m { return true }
            return false
        }
    }

    mod.draw.DrawFabBoard = mod.std.set_type_default() do #(DrawFabBoard::script_shader(vm)){
        ..mod.draw.FabBoardSurface,
        ..mod.draw.SdfLit,
        ..mod.draw.SunCascades,
        // After art and every other inherited texture, preserving their fixed slots.
        csm_map: texture_depth(float)
        // Linear colour and roughness of each layer; the mask's coat: f0, translucency, lift over
        // copper, scatter mm; layer_f0: silk (x), laminate (y) reflectance at normal incidence.
        // All from fab_materials' board_look (BoardLook::bind).
        mask: uniform(vec4(0.013, 0.147, 0.042, 0.18))
        mask_coat: uniform(vec4(0.043, 0.55, 1.6, 0.08))
        finish: uniform(vec4(1.0, 0.77, 0.36, 0.22))
        silk: uniform(vec4(0.87, 0.87, 0.83, 0.65))
        laminate: uniform(vec4(0.50, 0.44, 0.24, 0.6))
        layer_f0: uniform(vec4(0.04, 0.04, 0.0, 0.0))

        sun_visibility: fn(n: vec3) -> float {
            let ndl = dot(n, normalize(self.sun_dir))
            if ndl <= 0.0 { return 1.0 }
            return self.csm_vis(self.v_world, n, ndl)
        }

        // The mask: a scattering layer whose clear coat reflects on top with the parts' lobes.
        coated: fn(n: vec3f, v: vec3f, albedo: vec3f, vis: float) -> vec3f {
            let rough = clamp(self.mask.w, 0.04, 1.0)
            let f0 = vec3(self.mask_coat.x, self.mask_coat.x, self.mask_coat.x)
            let f_coat = self.schlick(f0, max(dot(n, v), 0.0001))
            return albedo * self.diffuse_light_vis(n, vis) * (vec3(1.0, 1.0, 1.0) - f_coat)
                + self.spec_sun_vis(n, v, f0, rough, vis) + self.spec_env(n, v, f0, rough)
        }

        pixel: fn() -> vec4f {
            let uv = ((self.v_uv - self.art_rect.xy) / self.art_rect.z * 256.0 + vec2(2.0, 2.0)) / 260.0
            let a = self.art_at(uv)
            let fade = self.relief_fade()
            let v = self.eye_dir()
            let n = self.surface_normal(uv, a, fade, v)
            if self.clipped(self.v_obj) { discard() }
            if self.v_kind < 1.5 && a.w > 0.5 && a.y > 0.5 { discard() }
            let vis = self.sun_visibility(n)
            var color = vec3(0.0, 0.0, 0.0)
            if self.v_kind > 2.5 && self.v_plated > 0.5 {
                color = self.lit_surface_vis(n, v, self.finish.xyz, self.finish.w, 1.0, vis)
            } else if self.v_kind > 1.5 {
                let plies = 0.88 + 0.12 * (0.5 + 0.5 * sin(self.v_t * 6.2832 * 8.0))
                var albedo = self.laminate.xyz * plies
                if self.v_t < 0.015 || self.v_t > 0.985 { albedo = self.mask.xyz }
                color = self.lit_surface_f0_vis(n, v, albedo, self.laminate.w, 0.0, self.layer_f0.y, vis)
            } else {
                let d = vec2(1.5 / 260.0, 1.5 / 260.0)
                let cu_soft = (self.art_at(uv + vec2(d.x, d.y)).x
                    + self.art_at(uv + vec2(d.x, 0.0 - d.y)).x
                    + self.art_at(uv + vec2(0.0 - d.x, d.y)).x
                    + self.art_at(uv - d).x) * 0.25
                let bare = mix(self.lit_surface_f0_vis(n, v, self.laminate.xyz, self.laminate.w, 0.0, self.layer_f0.y, vis),
                    self.lit_surface_vis(n, v, self.finish.xyz, self.finish.w, 1.0, vis), a.x)
                // Over copper the mask reads lifted; a tent over a drill has nothing under it and
                // reads darker (its dimple is the copper's absence in the relief).
                let tent = a.w * (1.0 - a.y)
                let masked = self.coated(n, v, self.mask.xyz * mix(1.0, self.mask_coat.z, cu_soft) * (1.0 - 0.5 * tent), vis)
                color = mix(mix(masked, bare, a.y), self.lit_surface_f0_vis(n, v, self.silk.xyz, self.silk.w, 0.0, self.layer_f0.x, vis), a.z)
            }
            color = self.to_display(color * self.exposure)
            if self.display.x > 0.5 { color = self.output_dither(color, self.v_world) }
            return vec4(color, 1.0)
        }

        fragment: fn() { self.fb0 = self.pixel() }
    }

    mod.draw.DrawFabBoardAux = mod.std.set_type_default() do #(DrawFabBoardAux::script_shader(vm)){
        ..mod.draw.FabBoardSurface,
        ..mod.draw.FabAuxOut,

        pixel: fn() -> vec4f {
            let uv = ((self.v_uv - self.art_rect.xy) / self.art_rect.z * 256.0 + vec2(2.0, 2.0)) / 260.0
            let a = self.art_at(uv)
            let fade = self.relief_fade()
            let n = self.surface_normal(uv, a, fade, self.eye_dir())
            if self.clipped(self.v_obj) { discard() }
            if self.v_kind < 1.5 && a.w > 0.5 && a.y > 0.5 { discard() }
            let vn = normalize((self.draw_pass.camera_view * vec4(n, 0.0)).xyz)
            let e = self.oct_encode(vn)
            return vec4(length(self.v_view), e.x, e.y, self.code)
        }

        fragment: fn() { self.fb0 = self.pixel() }
    }
}

/// Board millimetres, with Z from the bottom face to the top face.
#[repr(C)]
#[derive(Clone, Script, ScriptHook)]
pub struct BoardVertex {
    #[live]
    pub pos: Vec3f,
    /// Top, bottom, edge wall, hole barrel: 0, 1, 2, 3.
    #[live]
    pub kind: f32,
    #[live]
    pub normal: Vec3f,
    #[live]
    pub plated: f32,
    #[live]
    pub uv: Vec2f,
    #[live]
    pub t: f32,
    #[live]
    pub pad: f32,
}

const _: () = assert!(std::mem::size_of::<BoardVertex>() == 48);

fn board_vertex_pod(vm: &mut ScriptVm) -> ScriptValue {
    let v = BoardVertex::script_pod(vm).expect("BoardVertex pod");
    vm.heap_mut().pod_type_name_set(v, id_lut!(BoardVertex));
    v.into()
}

fn board_placeholder_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(BoardGeom), |cx| {
        let g = Geometry::new(cx);
        let mut v = Vec::new();
        for p in [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]] {
            v.extend_from_slice(&[p[0], p[1], 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, p[0], p[1], 0.0, 0.0]);
        }
        g.update(cx, vec![0, 1, 2], v);
        g
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

macro_rules! board_draw {
    ($name:ident) => {
        #[derive(Script, ScriptHook, Debug)]
        #[repr(C)]
        pub struct $name {
            #[deref]
            pub draw_vars: DrawVars,
            /// Homogeneous position W; the draw API requires at least one instance slot.
            #[live(1.0)]
            pub vertex_w: f32,
        }

        impl $name {
            fn draw(&mut self, cx: &mut CxDraw, geometry_id: GeometryId) -> u32 {
                self.vertex_w = 1.0;
                self.draw_vars.geometry_id = Some(geometry_id);
                if !self.draw_vars.can_instance() || cx.new_draw_call(&mut self.draw_vars).is_none() {
                    return 0;
                }
                let new_area = cx.add_instance(&mut self.draw_vars);
                self.draw_vars.area = cx.update_area_refs(self.draw_vars.area, new_area);
                1
            }
        }
    };
}
board_draw!(DrawFabBoard);
board_draw!(DrawFabBoardAux);

/// The shader's uniforms for a board's look, resolved by `fab_materials::shade::board_look`:
/// linear colours with roughness, the mask's coat (f0, translucency, lift over copper, scatter mm),
/// silk and laminate reflectance, and stack thicknesses in millimetres (board, copper, mask over
/// laminate, silk).
pub struct BoardLook {
    pub mask: [f32; 4],
    pub mask_coat: [f32; 4],
    pub finish: [f32; 4],
    pub silk: [f32; 4],
    pub laminate: [f32; 4],
    /// Silk and laminate reflectance at normal incidence (x and y); z and w are unused.
    pub layer_f0: [f32; 4],
    pub stack: [f32; 4],
}

impl BoardLook {
    pub fn resolve(look: &fab_materials::shade::BoardLook, thickness_mm: f32, copper_mm: f32) -> Self {
        let rgb_r = |c: [f32; 3], r: f32| [c[0], c[1], c[2], r];
        Self {
            mask: rgb_r(look.mask.colour, look.mask.roughness),
            mask_coat: [look.mask.f0, look.mask.translucency, look.mask.lift, look.mask.scatter_mm],
            finish: rgb_r(look.copper.base, look.copper.roughness),
            silk: rgb_r(look.silk.colour, look.silk.roughness),
            laminate: rgb_r(look.laminate.base, look.laminate.roughness),
            layer_f0: [look.silk.f0, look.laminate.f0[0], 0.0, 0.0],
            stack: [thickness_mm, copper_mm, look.mask.thickness_mm[1], look.silk.thickness_mm[0]],
        }
    }
}

impl BoardLook {
    pub fn bind(&self, vars: &mut DrawVars, cx: &mut Cx) {
        for (name, value) in [(id!(mask), self.mask), (id!(mask_coat), self.mask_coat), (id!(finish), self.finish), (id!(silk), self.silk), (id!(laminate), self.laminate), (id!(layer_f0), self.layer_f0), (id!(stack), self.stack)] {
            vars.set_uniform(cx, name, &value);
        }
    }
}

struct PreparedBoards {
    boards: Vec<SceneBoardPrep>,
    scene: Arc<ElecScene>,
    /// The scene was built from the committed design (`AppState::elec_scene_committed`).
    committed: bool,
}

type TileKey = (bool, TileId);

const FACE_TILE_IDLE_SECONDS: f64 = 10.0;

struct CachedFace {
    geometry: Option<Geometry>,
    geometry_built: bool,
    selected: bool,
    last_used: f64,
}

impl CachedFace {
    fn new(now: f64) -> Self {
        Self { geometry: None, geometry_built: false, selected: false, last_used: now }
    }
}

fn surface_plane(bottom: bool) -> ArtPlane {
    ArtPlane::Surface(if bottom { Side::Bottom } else { Side::Top })
}

/// Publish the recorded face selection for the host's shared art cache; `begin` opens the
/// drawing's frame (the first pane's selection), later panes add to its wants.
fn want_art<T: Clone>(art: &mut BoardArt<T>, roots: &[TileId], wanted: &[Vec<TileId>; 2], begin: bool) {
    if begin {
        art.begin_frame();
    }
    for (bottom, tiles) in [false, true].into_iter().zip(wanted) {
        let plane = surface_plane(bottom);
        art.want_tiles(plane, roots);
        art.want_tiles(plane, tiles);
    }
}

/// The resident art that covers `tile`, listed in `out`, which the caller keeps across frames.
fn cover_art<'a, T: Clone>(art: &BoardArt<T>, bottom: bool, tile: TileId, out: &'a mut Vec<LayerArt<T>>) -> Option<&'a LayerArt<T>> {
    let mut rect = tile.rect_nm();
    // cover uses closed nm boxes: exclude the neighbouring row/column at the maximum edge.
    rect.max.x = rect.max.x.saturating_sub(1);
    rect.max.y = rect.max.y.saturating_sub(1);
    art.cover_into(surface_plane(bottom), tile.lod, rect, out);
    out.first()
}

/// Camera terms shared by every node visited in the selection quadtree.
struct TileView {
    bounds: Aabb,
    eye_mm: [f64; 3],
    clip_from_board: Mat4f,
    pixels_per_m: f64,
    perspective: bool,
}

impl TileView {
    fn visit(&self, tile: TileId, z: f64, selected: &mut Vec<TileId>) {
        let rect = tile.rect_nm();
        let min = [rect.min.x as f64 * 1e-6, rect.min.y as f64 * 1e-6];
        let max = [rect.max.x as f64 * 1e-6, rect.max.y as f64 * 1e-6];
        if max[0] <= self.bounds.min.x as f64 || min[0] >= self.bounds.max.x as f64
            || max[1] <= self.bounds.min.y as f64 || min[1] >= self.bounds.max.y as f64 {
            return;
        }
        let corners = [[min[0], min[1]], [max[0], min[1]], [min[0], max[1]], [max[0], max[1]]]
            .map(|p| self.clip_from_board.transform_vec4(vec4(p[0] as f32, p[1] as f32, z as f32, 1.0)));
        // The camera's GPU clip depth is 0..w for both projection kinds.
        if (0..6).any(|plane| corners.iter().all(|p| {
            let margins = [p.w + p.x, p.w - p.x, p.w + p.y, p.w - p.y, p.z, p.w - p.z];
            margins[plane] < -8.0 * f32::EPSILON * p.w.abs().max(1e-6)
        })) {
            return;
        }
        let dx = self.eye_mm[0] - self.eye_mm[0].clamp(min[0], max[0]);
        let dy = self.eye_mm[1] - self.eye_mm[1].clamp(min[1], max[1]);
        let dz = self.eye_mm[2] - z;
        let distance_m = ((dx * dx + dy * dy + dz * dz).sqrt() * 0.001).max(1e-9);
        let density = if self.perspective { self.pixels_per_m / distance_m } else { self.pixels_per_m };
        let texel_m = ((ART_BASE_NM as i64) << tile.lod) as f64 * 1e-9;
        if tile.lod > 0 && texel_m * density > 1.0 {
            for y in 0..2 {
                for x in 0..2 {
                    self.visit(TileId { lod: tile.lod - 1, x: tile.x * 2 + x, y: tile.y * 2 + y }, z, selected);
                }
            }
        } else {
            selected.push(tile);
        }
    }
}

/// Selection is O(boards + selected tiles) per recorded frame (bounded quadtree depth).
/// Both passes reuse it: one draw per selected tile and side, or one whole face mesh
/// without art, plus each board's walls and barrels. Face cache misses clip on the
/// UI thread; the electronics host supplies shared art. Scene and body preparation run
/// on the pool; timeout-driven geometry expiry does not redraw the view.
pub struct BoardLayer {
    boards: Vec<BoardEntry>,
    bodies: bodies::BodyLayer,
    /// Last host publication: identical held sets do no body work on sync.
    held: Option<Arc<Vec<fab_elec_core::CompRef>>>,
    /// Last turned places and knob starts: unchanged ones do no body work on sync.
    knobs: Option<(Arc<Vec<(fab_elec_core::CompRef, f64)>>, Arc<fab_elec_bench::ValueIndex>)>,
    pub(super) casters: Rc<RefCell<casters::ElecCasters>>,
    /// Scene generation and plate bounds used to resolve the bodies' shared bed look.
    bed_key: Option<(u64, [f64; 2], [f64; 2])>,
    pending: Option<TaskHandle<PreparedBoards>>,
    /// When the pending scene arrived (`Cx::monotonic_now`), for its preparation's timing.
    asked_at: f64,
    requested: Option<Arc<ElecScene>>,
    /// `requested` was built from the committed design.
    requested_committed: bool,
    scene: Option<Arc<ElecScene>>,
    spot: [f32; 2],
    /// The pose of the boards no case part holds: their outline union centred on the spot.
    plate_from_scene: Mat4f,
    /// Each scene board's rest placement (scene mm to plate mm), by board index.
    poses: Vec<Mat4f>,
    /// Per scene board, the case part holding it (`held_boards`), and the plate, the installed
    /// scene (its elements) and the board scene it was looked up for: one lookup per change of
    /// any, none per frame.
    links: Vec<Option<(Mat4f, Option<ElementId>)>>,
    links_key: Option<(u64, u64, usize)>,
    /// Visibility from the holding parts, looked up with `links`.
    parts_hidden: bool,
    /// Resolving an archived job can scan the job list. Only its print revision in Monitor matters.
    monitor_revision: Option<u64>,
    /// The holding parts hide the boards or Monitor holds a job. Nothing draws, casts or picks.
    hidden: bool,
    /// The holding parts of the boards drawn: their own meshes stay out of the plate passes.
    drawn_parts: Vec<ElementId>,
    empty_art: Option<Texture>,
    planes: [[f32; 4]; 6],
    plane_count: usize,
    /// The drawing whose first pane opened the boards' art frames: every pane's selection of that
    /// drawing adds its wants, so the panes' tiles are all owed together.
    art_frame: Option<u64>,
}

/// Layer-art tiles each pane rasters per draw, coarse first: a frame budget (owed tiles wait for
/// the next frame while coarser resident art covers them). At the 100k-item scale one tile costs
/// about a frame of vertex work.
const ART_TILES_PER_DRAW: usize = 2;
/// Resident art a board keeps beyond what the panes want, as the board panes keep it.
const ART_BUDGET_BYTES: usize = 256 << 20;

impl Default for BoardLayer {
    fn default() -> Self {
        let casters = Rc::new(RefCell::new(casters::ElecCasters::default()));
        Self {
            boards: Vec::new(),
            bodies: bodies::BodyLayer::with_casters(casters.clone()),
            held: None,
            knobs: None,
            casters,
            bed_key: None,
            pending: None,
            asked_at: 0.0,
            requested: None,
            requested_committed: true,
            scene: None,
            spot: [0.0; 2],
            plate_from_scene: Mat4f::identity(),
            poses: Vec::new(),
            links: Vec::new(),
            links_key: None,
            parts_hidden: false,
            monitor_revision: None,
            hidden: false,
            drawn_parts: Vec::new(),
            empty_art: None,
            planes: [[0.0; 4]; 6],
            plane_count: 0,
            art_frame: None,
        }
    }
}

/// The box around `b` under `m`; None for an empty box.
fn transformed_bounds(b: &Aabb, m: &Mat4f) -> Option<Aabb> {
    if aabb_is_empty(b) { return None; }
    let mut result = aabb_empty();
    for x in [b.min.x, b.max.x] {
        for y in [b.min.y, b.max.y] {
            for z in [b.min.z, b.max.z] {
                let p = m.transform_vec4(vec4(x, y, z, 1.0));
                result.min.x = result.min.x.min(p.x);
                result.min.y = result.min.y.min(p.y);
                result.min.z = result.min.z.min(p.z);
                result.max.x = result.max.x.max(p.x);
                result.max.y = result.max.y.max(p.y);
                result.max.z = result.max.z.max(p.z);
            }
        }
    }
    Some(result)
}

/// Per scene board, the plate part holding it: a part linked `board:<name>` with that frame
/// (`LinkedPlace`), as a case holds its board. The board's plate pose (board mm to plate mm)
/// and the part's element. O(boards x parts), once per plate or scene change.
fn held_boards(scene: &ElecScene, parts: &[PlatePart]) -> Vec<Option<(Mat4f, Option<ElementId>)>> {
    scene.boards.iter().map(|board| {
        parts.iter().find_map(|part| {
            let place = part.linked.as_ref()?;
            (place.key.strip_prefix("board:") == Some(board.name.as_str())).then(|| (rows_mat(&place.plate_from_frame), part.element))
        })
    }).collect()
}

/// The plate holds some scene board in a part (`held_boards`), and every such part's placement
/// says `hidden: true`: the bar's board toggle (`AppState::board_shown`) has the layer off. The
/// boards of one scene show or hide together, a board no part holds beside held ones too (a
/// design's other units, which draw nothing in place). A scene no part holds (the Electronics
/// view's free boards) always shows. O(boards x parts), with `held_boards`.
fn boards_hidden(scene: &ElecScene, parts: &[PlatePart]) -> bool {
    let holder = |name: &str| parts.iter().find(|part| part.linked.as_ref().is_some_and(|place| place.key.strip_prefix("board:") == Some(name)));
    let mut held = scene.boards.iter().filter_map(|board| holder(&board.name)).peekable();
    held.peek().is_some() && held.all(|part| part.hidden)
}

/// A row-major affine map (`[[f64; 4]; 3]`, the plate's frames) as a matrix.
fn rows_mat(m: &[[f64; 4]; 3]) -> Mat4f {
    let mut v = [0.0f32; 16];
    for (c, column) in v.chunks_exact_mut(4).enumerate() {
        for r in 0..3 {
            column[r] = m[r][c] as f32;
        }
    }
    v[15] = 1.0;
    Mat4f { v }
}

/// Holder poses use Fab metres. The board and body placements use plate millimetres.
fn holder_move_mm(pose: crate::model::state::Pose) -> Mat4f {
    let mm = pose.map(|row| std::array::from_fn(|c| f64::from(row[c]) * if c == 3 { 1000.0 } else { 1.0 }));
    rows_mat(&mm)
}

pub(super) fn moved_pose(rest: Mat4f, movement: Option<Mat4f>) -> Mat4f {
    movement.map_or(rest, |mv| Mat4f::mul(&mv, &rest))
}

/// Identifies a pane's board moves for both its shadows and its LED pools.
pub(super) fn moves_hash(moves: &[Option<Mat4f>]) -> u64 {
    if moves.is_empty() { return 0; }
    let mut hash = DefaultHasher::new();
    for (board, movement) in moves.iter().enumerate() {
        board.hash(&mut hash);
        movement.as_ref().map(|m| m.v.map(f32::to_bits)).hash(&mut hash);
    }
    hash.finish()
}

/// One board's retained geometry, appearance and viewer of shared layer art.
struct BoardEntry {
    unit: BoardId,
    edges: Rc<Geometry>,
    /// The CPU faces the art tiles clip; the whole face mesh draws until the coarse art is in.
    faces: Option<BoardMesh>,
    /// Whole slab faces stay alive with the board, shared by its sun caster even when art is tiled.
    face_geometry: Option<Rc<Geometry>>,
    fab_from_board: Mat4f,
    scene_from_board: Mat4f,
    /// The board's layer art, rastered from its shared drawing (`BoardDraws`) once one is in.
    art: Option<BoardArt<Texture>>,
    /// The shared drawing's generation the art was advanced to.
    art_gen: Option<u64>,
    tiles: HashMap<TileKey, CachedFace>,
    roots: Vec<TileId>,
    wanted: [Vec<TileId>; 2],
    /// Face geometry key and the covering resident tile; both passes use this selection.
    selected: Vec<(TileKey, TileId)>,
    /// The cover of one selected tile, kept so selection allocates nothing in the steady state.
    cover: Vec<LayerArt<Texture>>,
    evict_timer: Timer,
    look: BoardLook,
    object_bounds: Aabb,
    /// Slab at rest in plate millimetres.
    rest_bounds: Option<Aabb>,
}

impl BoardEntry {
    fn new(cx: &mut Cx, prep: SceneBoardPrep, plate_from_scene: &Mat4f, casters: &mut casters::ElecCasters) -> Self {
        casters.ensure_shaders(cx);
        let edges = Geometry::new(cx);
        edges.update(cx, prep.edges.indices, prep.edges.vertices);
        let face_geometry = Geometry::new(cx);
        face_geometry.update(cx, prep.faces.indices.clone(), prep.faces.vertices.clone());
        Self {
            unit: prep.unit,
            edges: Rc::new(edges),
            faces: Some(prep.faces),
            face_geometry: Some(Rc::new(face_geometry)),
            fab_from_board: Mat4f::mul(plate_from_scene, &prep.scene_from_board),
            scene_from_board: prep.scene_from_board,
            art: None,
            art_gen: None,
            tiles: HashMap::new(),
            roots: prep.roots,
            wanted: [Vec::new(), Vec::new()],
            selected: Vec::new(),
            cover: Vec::new(),
            evict_timer: Timer::empty(),
            look: prep.look,
            object_bounds: prep.bounds,
            rest_bounds: None,
        }
    }

    /// Whole faces are ready immediately and draw until the art is in.
    fn drawable(&self) -> bool {
        self.tiled() || self.face_geometry.is_some()
    }

    /// Tiled faces wait for both sides' coarse coverage, so every selected face has an ancestor
    /// in its first frame.
    fn tiled(&self) -> bool {
        let Some(art) = &self.art else { return false };
        self.faces.is_some() && [false, true].into_iter().all(|bottom| self.roots.iter().all(|tile| {
            art.tile(surface_plane(bottom), *tile).is_some()
        }))
    }

    /// Expiry frees unused face geometry while the camera rests, without recording a new frame
    /// or evicting its selected meshes.
    fn expire(&mut self, cx: &mut Cx, event: &Event) {
        if self.evict_timer.is_event(event).is_some() {
            self.evict_timer = Timer::empty();
            self.evict_unused(cx);
        }
    }

    fn evict_unused(&mut self, cx: &mut Cx) {
        let now = Cx::time_now();
        let before = self.tiles.len();
        let mut next = f64::INFINITY;
        self.tiles.retain(|_, entry| {
            if entry.selected { return true; }
            let remaining = FACE_TILE_IDLE_SECONDS - (now - entry.last_used);
            if remaining > 0.0 {
                next = next.min(remaining);
                return true;
            }
            false
        });
        let count = before - self.tiles.len();
        if count != 0 { log!("board face tiles: evicted {count}"); }
        if next.is_finite() {
            self.evict_timer = cx.start_timeout(next.max(0.01));
        }
    }

    /// Called once, inside the aux pass, before either pass draws this recording.
    /// Its pass size times DPI is the actual raster height, including a jittered
    /// recording's projection. The lit pass consumes the same immutable list.
    fn select(&mut self, cx: &mut CxDraw, scene: &SceneState3D, begin: bool, movement: Option<Mat4f>) -> bool {
        if self.art.is_none() { return self.drawable(); }
        let now = Cx::time_now();
        // A retained recording stays selected while the camera rests. Timestamp
        // its keys when it is replaced so expiry starts when a tile leaves view.
        for (bottom, wanted) in [false, true].into_iter().zip(&mut self.wanted) {
            for &tile in wanted.iter() {
                if let Some(entry) = self.tiles.get_mut(&(bottom, tile)) {
                    entry.last_used = now;
                    entry.selected = false;
                }
            }
            wanted.clear();
        }
        self.selected.clear();
        if !self.tiled() {
            want_art(self.art.as_mut().unwrap(), &self.roots, &self.wanted, begin);
            return self.drawable();
        }
        let art = self.art.as_ref().unwrap();
        let bounds = self.object_bounds;
        let fab_from_board = moved_pose(self.fab_from_board, movement);
        let eye = fab_from_board.invert().transform_vec4(vec4(
            scene.camera_pos.x * 1000.0, scene.camera_pos.y * 1000.0, scene.camera_pos.z * 1000.0, 1.0,
        ));
        let fab = Mat4f::mul(&Mat4f::scale(0.001), &fab_from_board);
        let view = TileView {
            bounds,
            eye_mm: [eye.x as f64, eye.y as f64, eye.z as f64],
            clip_from_board: Mat4f::mul(&scene.projection, &Mat4f::mul(&scene.view, &fab)),
            pixels_per_m: scene.projection.v[5].abs() as f64 * (cx.current_pass_size().y * cx.current_dpi_factor()).max(1.0) * 0.5,
            perspective: scene.projection.v[11] != 0.0,
        };
        for (bottom, z) in [(false, self.look.stack[0] as f64), (true, 0.0)] {
            if (bottom && eye.z > self.look.stack[0]) || (!bottom && eye.z < 0.0) { continue; }
            let wanted = &mut self.wanted[usize::from(bottom)];
            for &root in &self.roots {
                view.visit(root, z, wanted);
            }
            for &tile in wanted.iter() {
                let key = (bottom, tile);
                let entry = self.tiles.entry(key).or_insert_with(|| CachedFace::new(now));
                entry.last_used = now;
                entry.selected = true;
                if !entry.geometry_built {
                    // UI clipping cost per tile: awaiting the lead's JIT measurement.
                    let grow_mm = TileId::span(tile.lod) as f64 * 1e-6 / ART_TILE_TEXELS as f64;
                    let mesh = face_tile(self.faces.as_ref().unwrap(), tile, bottom, grow_mm);
                    if !mesh.indices.is_empty() {
                        let geometry = Geometry::new(cx);
                        geometry.update(cx, mesh.indices, mesh.vertices);
                        entry.geometry = Some(geometry);
                    }
                    entry.geometry_built = true;
                }
                if entry.geometry.is_none() { continue; }
                if let Some(cover) = cover_art(art, bottom, tile, &mut self.cover) {
                    self.selected.push((key, cover.tile));
                }
            }
        }
        want_art(self.art.as_mut().unwrap(), &self.roots, &self.wanted, begin);
        if self.evict_timer.is_empty() {
            self.evict_timer = cx.start_timeout(FACE_TILE_IDLE_SECONDS);
        }
        true
    }

    /// Placed board bounds in scene millimetres.
    fn bounds(&self) -> Option<Aabb> {
        transformed_bounds(&self.object_bounds, &self.scene_from_board)
    }

    /// Walls and barrels first, then the whole faces or each selected tile and side.
    /// Aux uses the plate world; lit uses the render frame and the light's units.
    fn draw(&self, cx: &mut Cx3d, lit: Option<(&TileLight, &Renderer)>, aux: &mut DrawFabBoardAux, shade: &mut DrawFabBoard,
        empty_art: Option<&Texture>, planes: &[[f32; 4]; 6], plane_count: usize, movement: Option<Mat4f>) -> u32 {
        let Some(scene) = cx.scene_state_3d() else { return 0 };
        let geometry = &self.edges;
        if !self.drawable() { return 0; }
        let fab = Mat4f::mul(&Mat4f::scale(0.001), &moved_pose(self.fab_from_board, movement));
        let world = if lit.is_some() { Mat4f::mul(&render_from_fab(), &fab) } else { fab };
        let height_px = (cx.current_pass_size().y * cx.current_dpi_factor()).max(1.0) as f32;
        let half_pixel = 1.0 / (scene.projection.v[5].abs() * height_px);
        let vars = if lit.is_some() { &mut shade.draw_vars } else { &mut aux.draw_vars };
        vars.set_uniform(cx.cx, id!(world_from_obj), &world.v);
        vars.set_uniform(cx.cx, id!(art_rect), &[0.0, 0.0, 1.0, 0.0]);
        vars.set_uniform(cx.cx, id!(stack), &self.look.stack);
        vars.set_uniform(cx.cx, id!(code), &[16000008.0]);
        // Supply a valid binding even on the first recording; wall/barrel code
        // never samples it. Tiled faces replace it with their own or ancestor's art;
        // whole faces keep the shared zero-coverage texture.
        let tiled = self.tiled();
        let texture = match &self.art {
            Some(art) if tiled => self.roots.first().and_then(|tile| art.tile(surface_plane(false), *tile)).map(|resident| &resident.texture),
            _ => empty_art,
        };
        if let Some(texture) = texture {
            set_texture_named(vars, cx.cx, id!(art), texture);
        }
        vars.set_uniform(cx.cx, id!(clip_ctl), &[plane_count as f32, half_pixel, if scene.projection.v[11] == 0.0 { 0.0 } else { 1.0 }, 0.0]);
        // Object half-spaces are identical in both passes: M^T times the plate plane.
        for (name, plane) in [id!(clip0), id!(clip1), id!(clip2), id!(clip3), id!(clip4), id!(clip5)].into_iter().zip(*planes) {
            let m = &fab.v;
            let mut p = [0.0; 4];
            for j in 0..4 {
                p[j] = plane[0] * m[j * 4] + plane[1] * m[j * 4 + 1] + plane[2] * m[j * 4 + 2] + plane[3] * m[j * 4 + 3];
            }
            vars.set_uniform(cx.cx, name, &p);
        }
        if let Some((light, renderer)) = lit {
            self.look.bind(vars, cx.cx);
            vars.set_uniform(cx.cx, id!(sun_dir), &light.dir);
            vars.set_uniform(cx.cx, id!(sun_color), &light.color);
            vars.set_uniform(cx.cx, id!(sun_sky), &light.sky);
            vars.set_uniform(cx.cx, id!(sun_ground), &light.ground);
            vars.set_uniform(cx.cx, id!(world_up), &light.up);
            vars.set_uniform(cx.cx, id!(exposure), &[light.exposure]);
            vars.set_uniform(cx.cx, id!(display), &[if light.display { 1.0 } else { 0.0 }, light.sun_spec, 0.0, 0.0]);
            vars.set_uniform(cx.cx, id!(key_spec), &light.key_spec);
            vars.set_uniform(cx.cx, id!(key_tint), &light.key_tint);
            renderer.bind_ibl(cx.cx, vars);
            renderer.bind_sun_cascades(cx.cx, vars);
            renderer.bind_output_dither(cx.cx, vars);
        }
        let mut calls = if lit.is_some() { shade.draw(cx.cx, geometry.geometry_id()) } else { aux.draw(cx.cx, geometry.geometry_id()) };
        if let Some(geometry) = self.face_geometry.as_ref().filter(|_| !tiled) {
            calls += if lit.is_some() { shade.draw(cx.cx, geometry.geometry_id()) } else { aux.draw(cx.cx, geometry.geometry_id()) };
        }
        for &(key, art_tile) in &self.selected {
            let Some(geometry) = self.tiles.get(&key).and_then(|entry| entry.geometry.as_ref()) else { continue };
            let Some(art) = self.art.as_ref().and_then(|art| art.tile(surface_plane(key.0), art_tile)) else { continue };
            let rect = art.rect_nm;
            let vars = if lit.is_some() { &mut shade.draw_vars } else { &mut aux.draw_vars };
            // CxDrawCall copies dyn_uniforms and texture_slots at new_draw_call;
            // set_uniform changes only DrawVars, never a previously recorded tile.
            vars.set_uniform(cx.cx, id!(art_rect), &[rect.min.x as f32 * 1e-6, rect.min.y as f32 * 1e-6, (rect.max.x as f64 - rect.min.x as f64) as f32 * 1e-6, 0.0]);
            set_texture_named(vars, cx.cx, id!(art), &art.texture);
            calls += if lit.is_some() { shade.draw(cx.cx, geometry.geometry_id()) } else { aux.draw(cx.cx, geometry.geometry_id()) };
        }
        calls
    }
}

impl BoardLayer {
    pub(super) fn moves_under(&self, state: &AppState, poses: PanePoses, out: &mut Vec<Option<Mat4f>>) {
        out.clear();
        out.extend((0..self.boards.len()).map(|i| {
            let (_, part) = self.links.get(i)?.as_ref()?;
            state.pose_under(poses, (*part)?)
                .filter(|pose| *pose != crate::model::state::POSE_IDENTITY)
                .map(holder_move_mm)
        }));
    }

    pub fn sync(&mut self, cx: &mut Cx, state: &AppState) -> bool {
        let mut changed = false;
        self.bodies.set_soldered(state.view.soldered);
        let bed_key = (state.scene.generation, state.plate.bed_min, state.plate.bed_max);
        if self.bed_key != Some(bed_key) {
            let bed = bodies::bed_look(state);
            changed = self.bodies.bed != bed;
            self.bodies.bed = bed;
            self.bed_key = Some(bed_key);
        }
        let same = match (&self.requested, &state.elec_scene) {
            (Some(requested), Some(scene)) => Arc::ptr_eq(requested, scene),
            (None, None) => true,
            _ => false,
        };
        if !same {
            self.requested = state.elec_scene.clone();
            self.requested_committed = state.elec_scene_committed;
            if self.requested.is_some() {
                // One preparation at a time: a running one is never cancelled for a newer scene.
                // It lands and shows, and the newest scene asked for since starts the next.
                if self.pending.is_none() {
                    self.start_prepare(cx);
                }
            } else {
                if let Some(task) = self.pending.take() { task.cancel(); }
                self.boards.clear();
                self.casters.borrow_mut().set_boards(Vec::new());
                self.casters.borrow_mut().set_bodies(Vec::new());
                let bed = self.bodies.bed;
                self.bodies = bodies::BodyLayer::with_casters(self.casters.clone());
                self.held = None;
                self.bodies.bed = bed;
                self.scene = None;
                self.plate_from_scene = Mat4f::identity();
                self.poses.clear();
                self.links.clear();
                self.links_key = None;
                self.drawn_parts.clear();
                return true;
            }
        }
        if let Some(result) = self.pending.as_mut().and_then(|task| task.try_take()) {
            self.pending = None;
            match result {
                Ok(data) => {
                    log!("fab boards: {} prepared and drawn {:.0} ms after their scene arrived", data.boards.len(), (Cx::monotonic_now() - self.asked_at) * 1e3);
                    if self.empty_art.is_none() {
                        // All RGBA8 channels are zero; the platform's BGRA byte order is immaterial.
                        self.empty_art = Some(Texture::new_with_format(cx, TextureFormat::VecBGRAu8_32 {
                            width: 4,
                            height: 4,
                            data: Some(vec![0; 16]),
                            updated: TextureUpdated::Full,
                        }));
                    }
                    // A board keeps its layer art across new geometry: the next drawing
                    // generation re-rasters it while its resident tiles keep covering the faces.
                    let mut arts: HashMap<BoardId, (BoardArt<Texture>, Option<u64>)> = self.boards.drain(..)
                        .filter_map(|board| Some((board.unit, (board.art?, board.art_gen)))).collect();
                    self.boards = data.boards.into_iter().map(|prep| {
                        let mut board = BoardEntry::new(cx, prep, &self.plate_from_scene, &mut self.casters.borrow_mut());
                        if let Some((art, art_gen)) = arts.remove(&board.unit) {
                            board.art = Some(art);
                            board.art_gen = art_gen;
                        }
                        board
                    }).collect();
                    self.casters.borrow_mut().set_boards(self.boards.iter().map(|board| {
                        casters::BoardCaster::new(board.edges.clone(), board.face_geometry.clone(), board.object_bounds, &board.fab_from_board)
                    }).collect());
                    // The new scene's bodies start at the free pose; `place` below gives each its
                    // board's (a held board's is its case part's frame). Forgetting the old poses
                    // makes it re-pose them when no board moved, as after every edit or undo.
                    // The first committed scene after a drag poses its bodies itself, where the
                    // engine put them (a snapped or nudged drop too): the drag's poses go.
                    if data.committed && !state.elec_previewing {
                        self.bodies.clear_live();
                    }
                    self.bodies.set_scene(cx, &data.scene, vec![self.plate_from_scene; data.scene.boards.len()]);
                    self.poses.clear();
                    let newer = self.requested.as_ref().is_some_and(|requested| !Arc::ptr_eq(requested, &data.scene));
                    self.scene = Some(data.scene);
                    changed = true;
                    if newer {
                        self.start_prepare(cx);
                    }
                }
                Err(error) => log!("board generation: {error}"),
            }
        }
        if self.bodies.set_live_poses(state.elec_poses.as_ref()) {
            changed = true;
        }
        let held = cx.get_global_ref::<fab_elec_view::ElecLive>().map(|live| &live.sim.held);
        if self.held_changed(held) {
            changed |= self.bodies.set_held(held.map_or(&[], |held| held.as_slice()));
            self.held = held.cloned();
        }
        let knobs = cx.get_global_ref::<fab_elec_view::ElecLive>().map(|live| (live.sim.turned.clone(), live.sim.index.clone()));
        let same_knobs = match (&self.knobs, &knobs) {
            (Some((t, i)), Some((u, j))) => Arc::ptr_eq(t, u) && Arc::ptr_eq(i, j),
            (None, None) => true,
            _ => false,
        };
        if !same_knobs {
            changed |= self.bodies.set_turns(knobs.as_ref().map_or_else(Vec::new, |(turned, index)| bodies::knob_angles(turned, index.knobs())));
            self.knobs = knobs;
        }
        // Only bodies uploaded for the current scene count in its placement bounds (`shows`).
        if self.bodies.sync(cx) {
            changed = true;
        }
        // A scene rebuild binds the parts to new elements with no plate edit (`AppState::update_scene`):
        // the holding part is looked up again, or its own mesh would draw through the board and a
        // Mechanics move would carry it away without the board.
        let links_key = (state.plate.revision, state.scene_revision, self.scene.as_ref().map_or(0, |scene| Arc::as_ptr(scene) as usize));
        let links_changed = self.links_key != Some(links_key);
        if links_changed {
            self.links_key = Some(links_key);
            let links = self.scene.as_ref().map_or_else(Vec::new, |scene| held_boards(scene, &state.plate.parts));
            if links != self.links {
                self.links = links;
                changed = true;
            }
            self.parts_hidden = self.scene.as_ref().is_some_and(|scene| boards_hidden(scene, &state.plate.parts));
        }
        let spot = state.elec_spot.map_or_else(
            || [(state.plate.bed_min[0] + state.plate.bed_max[0]) * 0.5, (state.plate.bed_min[1] + state.plate.bed_max[1]) * 0.5],
            |spot| spot,
        ).map(|v| v as f32);
        let place = changed || self.spot != spot;
        if place {
            self.spot = spot;
            changed |= self.place(cx);
            let mut casters = self.casters.borrow_mut();
            for (i, board) in self.boards.iter().enumerate() {
                casters.set_board_pose(i, &board.fab_from_board);
            }
        }
        let monitor_revision = state.print.follow.monitor.then_some(state.print.revision);
        if links_changed || self.monitor_revision != monitor_revision {
            self.monitor_revision = monitor_revision;
            let hidden = self.parts_hidden || (!self.boards.is_empty() && state.print.monitor_holds());
            if self.hidden != hidden {
                self.hidden = hidden;
                self.casters.borrow_mut().set_hidden(hidden);
                changed = true;
            }
        }
        changed
    }

    /// Prepare the requested scene's boards on the pool: the one preparation in flight.
    fn start_prepare(&mut self, cx: &mut Cx) {
        let Some(scene) = self.requested.clone() else { return };
        let committed = self.requested_committed;
        match cx.task_pool().submit_named(Class::Background, "fab-board-scene", move || {
            PreparedBoards { boards: prepare_scene(&scene), scene, committed }
        }) {
            Ok(task) => {
                self.pending = Some(task);
                self.asked_at = Cx::monotonic_now();
            }
            Err(error) => log!("board generation: {error}"),
        }
    }

    /// Fill the traced scene's poses (scene mm to plate mm) once the requested boards are placed.
    pub(super) fn board_poses<'a>(&self, moves: &[Option<Mat4f>], out: &'a mut Vec<Mat4f>) -> Option<&'a [Mat4f]> {
        out.clear();
        if self.hidden { return None; }
        let scene = self.scene.as_ref()?;
        let requested = self.requested.as_ref()?;
        if !Arc::ptr_eq(scene, requested) || self.poses.len() != scene.boards.len() { return None; }
        out.extend(self.poses.iter().enumerate().map(|(i, &rest)| moved_pose(rest, moves.get(i).copied().flatten())));
        Some(out)
    }

    /// Pose every board. One a case part holds (`held_boards`) stands in that part's frame of
    /// it; the others share one pose that centres their outline union on the free spot and
    /// rests their lowest board or body point at zero. Bodies follow their board, and the
    /// holding parts of the boards drawn leave the plate passes. O(boards) plus a body
    /// re-pose; true when what is drawn moved.
    fn place(&mut self, cx: &mut Cx) -> bool {
        let Some(scene) = self.scene.clone() else { return false };
        let held = |i: usize| self.links.get(i).and_then(|link| link.as_ref());
        let bodies_ready = self.bodies.shows(&scene);
        let (mut outline, mut rest) = (None::<Aabb>, None::<Aabb>);
        for (i, board) in self.boards.iter().enumerate() {
            if held(i).is_some() {
                continue;
            }
            for next in [board.bounds(), bodies_ready.then(|| self.bodies.board_bounds(i)).flatten()].into_iter().flatten() {
                rest = Some(rest.map_or(next, |rest| aabb_union(&rest, &next)));
            }
            if let Some(next) = board.bounds() {
                outline = Some(outline.map_or(next, |outline| aabb_union(&outline, &next)));
            }
        }
        let spot = self.spot;
        let free = outline.zip(rest).map_or(Mat4f::identity(), |(outline, rest)| {
            Mat4f::translation(vec3(
                spot[0] - (outline.min.x + outline.max.x) * 0.5,
                spot[1] - (outline.min.y + outline.max.y) * 0.5,
                -rest.min.z,
            ))
        });
        let mut fab_from_boards = Vec::with_capacity(self.boards.len());
        let poses: Vec<Mat4f> = self.boards.iter().enumerate().map(|(i, board)| match held(i) {
            // The holder's frame places the board at rest. Each pane supplies the holder's move.
            Some((plate_from_board, _)) => {
                fab_from_boards.push(*plate_from_board);
                Mat4f::mul(plate_from_board, &board.scene_from_board.invert())
            }
            None => {
                fab_from_boards.push(Mat4f::mul(&free, &board.scene_from_board));
                free
            }
        }).collect();
        let drawn_parts: Vec<ElementId> = (0..self.boards.len()).filter_map(|i| held(i).and_then(|(_, part)| *part)).collect();
        let same = |a: &[Mat4f], b: &[Mat4f]| a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.v == b.v);
        let moved = !same(&poses, &self.poses) || drawn_parts != self.drawn_parts;
        self.plate_from_scene = free;
        self.drawn_parts = drawn_parts;
        for (board, fab_from_board) in self.boards.iter_mut().zip(fab_from_boards) {
            board.fab_from_board = fab_from_board;
            board.rest_bounds = transformed_bounds(&board.object_bounds, &fab_from_board);
        }
        if moved {
            self.bodies.set_scene(cx, &scene, poses.clone());
            self.poses = poses;
        }
        moved
    }

    /// The plate parts whose meshes the boards stand in for (see `AppState::drawn_by_link`).
    pub fn drawn_parts(&self) -> &[ElementId] {
        &self.drawn_parts
    }

    /// A board or body build is in flight: what the layer draws is not yet the scene.
    pub(super) fn building(&self) -> bool {
        self.pending.is_some() || self.bodies.building()
    }

    pub(super) fn ready(&mut self, cx: &mut Cx, event: &Event) -> bool {
        if self.held_changed(cx.get_global_ref::<fab_elec_view::ElecLive>().map(|live| &live.sim.held)) {
            return true;
        }
        if matches!(event, Event::Timer(_)) {
            for board in &mut self.boards {
                board.expire(cx, event);
            }
        }
        if !matches!(event, Event::Signal) {
            return false;
        }
        // Whoever polls first takes the finished drawings: compare generations, not the result.
        BoardDraws::poll(cx);
        let new_art = self.boards.iter().any(|board| {
            BoardDraws::current(cx, board.unit).is_some_and(|draw| Some(draw.gen) != board.art_gen)
        });
        new_art || self.pending.as_ref().is_some_and(|task| task.is_finished()) || self.bodies.ready()
    }

    fn held_changed(&self, held: Option<&Arc<Vec<fab_elec_core::CompRef>>>) -> bool {
        match (&self.held, held) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        }
    }

    pub(super) fn press_at(&self, ray: &crate::model::query::Ray, moves: &[Option<Mat4f>]) -> Option<(fab_elec_core::CompRef, f32)> {
        if self.hidden { return None; }
        self.bodies.press_at(ray, moves)
    }

    /// The part whose body the ray meets first ([`bodies::BodyLayer::comp_at`]).
    pub(super) fn comp_at(&self, ray: &crate::model::query::Ray, moves: &[Option<Mat4f>]) -> Option<(fab_elec_core::CompRef, f32)> {
        if self.hidden { return None; }
        self.bodies.comp_at(ray, moves)
    }

    /// The bodies drawn from a model file ([`bodies::BodyLayer::model_bodies`]).
    pub(super) fn model_bodies(&self) -> usize {
        if self.hidden { 0 } else { self.bodies.model_bodies() }
    }

    /// A part's body centre in plate metres ([`bodies::BodyLayer::comp_centre`]).
    pub(super) fn comp_centre(&self, comp: fab_elec_core::CompRef, moves: &[Option<Mat4f>]) -> Option<Vec3f> {
        if self.hidden { return None; }
        self.bodies.comp_centre(comp, moves)
    }

    pub(super) fn turn_at(&self, ray: &crate::model::query::Ray, moves: &[Option<Mat4f>]) -> Option<(fab_elec_core::CompRef, f32)> {
        if self.hidden { return None; }
        self.bodies.turn_at(ray, moves)
    }

    /// The knobs' (`turn`) or buttons' grip points in plate metres ([`bodies::BodyLayer::port_tips`]).
    pub(super) fn port_tips(&self, turn: bool, moves: &[Option<Mat4f>]) -> Vec<(fab_elec_core::CompRef, Vec3f)> {
        if self.hidden { return Vec::new(); }
        self.bodies.port_tips(turn, moves)
    }

    pub(super) fn screen_at(&self, ray: &crate::model::query::Ray, held: Option<fab_elec_core::CompRef>, moves: &[Option<Mat4f>]) -> Option<(fab_elec_core::CompRef, f32, (f32, f32))> {
        if self.hidden { return None; }
        self.bodies.screen_at(ray, held, moves)
    }

    /// Feeds every board's layer art from its shared drawing (`BoardDraws::current`): a new
    /// drawing generation re-rasters the tiles its `changed` boxes meet (the whole board after a
    /// missed generation or a full build), and up to `ART_TILES_PER_DRAW` owed tiles are
    /// drawn, coarse first. Every pane calls it on every draw, so its raster also retires its
    /// finished GPU recordings. True while tiles are owed or were just drawn: the scene passes
    /// record again next frame. Cost: O(boards) plus the drawn tiles' raster passes.
    pub(super) fn raster_art(&mut self, cx: &mut Cx2d, raster: &mut ArtRaster) -> bool {
        if self.boards.is_empty() || !raster.ensure_templates(cx) {
            return false;
        }
        let mut again = false;
        for board in &mut self.boards {
            let Some(draw) = BoardDraws::current(cx, board.unit) else { continue };
            let bounds = art_box(&board.object_bounds);
            let art = board.art.get_or_insert_with(|| BoardArt::new(bounds, ART_BUDGET_BYTES));
            if board.art_gen != Some(draw.gen) {
                art.set_board(bounds);
                // The drawing after the one the art last drew says where it differs: only the
                // tiles those boxes meet re-raster (a drag redraws what it touched, the rest of
                // the board keeps showing). A missed drawing or a full build redraws the board.
                let whole = [bounds];
                let changed = match (&draw.changed, board.art_gen) {
                    (Some(boxes), Some(last)) if last + 1 == draw.gen => &boxes[..],
                    _ => &whole[..],
                };
                art.advance(draw.gen, changed);
                board.art_gen = Some(draw.gen);
            }
            let drawn = raster.raster_missing(cx, &draw, art, ART_TILES_PER_DRAW);
            drop(art.evict());
            again |= drawn > 0 || !art.missing().is_empty();
        }
        again
    }

    pub(super) fn drawable(&self) -> bool {
        !self.hidden && self.boards.iter().any(BoardEntry::drawable)
    }

    /// Select every board before either pass draws; reuse each entry's retained tile vectors.
    pub(super) fn select(&mut self, cx: &mut CxDraw, scene: &SceneState3D, moves: &[Option<Mat4f>]) -> bool {
        if self.hidden { return false; }
        let drawing = cx.drawing_id();
        let begin = self.art_frame != Some(drawing);
        self.art_frame = Some(drawing);
        let mut selected = false;
        for (i, board) in self.boards.iter_mut().enumerate() {
            selected |= board.select(cx, scene, begin, moves.get(i).copied().flatten());
        }
        selected
    }

    /// The viewport supplies section half-spaces in its Z-up world, in metres.
    pub(super) fn set_clip_planes(&mut self, planes: &[[f32; 4]]) {
        self.bodies.set_clip_planes(planes);
        self.plane_count = planes.len().min(6);
        self.planes = [[0.0; 4]; 6];
        self.planes[..self.plane_count].copy_from_slice(&planes[..self.plane_count]);
    }

    /// The first board's mid-plane, `n dot p + d = 0`, in Z-up Fab metres.
    /// Board-local z spans 0..thickness in millimetres. Transform its midpoint and use
    /// the inverse-transpose normal so a placed/tilted board reflects about its own plane.
    pub(super) fn mirror_plane(&self, moves: &[Option<Mat4f>]) -> Option<[f32; 4]> {
        if self.hidden { return None; }
        let first = self.boards.first()?;
        let fab_from_board = moved_pose(first.fab_from_board, moves.first().copied().flatten());
        let point = fab_from_board.transform_vec4(vec4(0.0, 0.0, first.look.stack[0] * 0.5, 1.0));
        let inverse = fab_from_board.invert();
        let normal = vec3(inverse.v[2], inverse.v[6], inverse.v[10]).normalize();
        let point = vec3(point.x, point.y, point.z) * 0.001;
        Some([normal.x, normal.y, normal.z, -normal.dot(point)])
    }

    /// Slab and prepared body bounds in the pane's plate frame, in millimetres.
    pub fn bounds(&self, moves: &[Option<Mat4f>]) -> Option<Aabb> {
        if self.hidden { return None; }
        let bodies_ready = self.scene.as_ref().is_some_and(|scene| self.bodies.shows(scene));
        self.boards.iter().enumerate().flat_map(|(i, board)| {
            let bodies = bodies_ready.then(|| self.bodies.plate_bounds(i)).flatten();
            match moves.get(i).copied().flatten() {
                Some(mv) => [
                    transformed_bounds(&board.object_bounds, &Mat4f::mul(&mv, &board.fab_from_board)),
                    bodies.and_then(|rest| transformed_bounds(&rest, &mv)),
                ],
                None => [board.rest_bounds, bodies],
            }
        }).flatten().reduce(|a, b| aabb_union(&a, &b))
    }

    /// Every board binds its own transform, stack and look in each pass.
    pub fn draw(&self, cx: &mut Cx3d, lit: Option<(&TileLight, &Renderer)>, aux: &mut DrawFabBoardAux, shade: &mut DrawFabBoard, moves: &[Option<Mat4f>]) -> u32 {
        self.boards.iter().enumerate().map(|(i, board)| {
            board.draw(cx, lit, aux, shade, self.empty_art.as_ref(), &self.planes, self.plane_count, moves.get(i).copied().flatten())
        }).sum()
    }

    /// `selected`: the parts the Electronics views selected, ringed as the active selection.
    pub fn draw_bodies(&mut self, cx: &mut Cx3d, lit: Option<(&TileLight, &Renderer)>, ao: Option<(&Texture, f32)>, bed_shown: bool, aux: &mut bodies::DrawFabBodyAux, shade: &mut bodies::DrawFabBody, moves: &[Option<Mat4f>], selected: &[fab_elec_core::CompRef]) -> u32 {
        self.bodies.draw(cx, lit, ao, bed_shown, aux, shade, moves, selected)
    }

    /// The drawn bodies have an LED lens that reads the live part rows ([`bodies::BodyLayer::emitting_lens`]).
    pub fn bodies_emitting_lens(&self) -> bool {
        !self.hidden && self.bodies.emitting_lens()
    }

    pub fn has_screen_glass(&self) -> bool {
        !self.hidden && self.bodies.has_screen_glass()
    }

    /// Lens batches in aux's frame, for the independent glow layer.
    pub fn draw_body_glow(&mut self, cx: &mut Cx3d, aux: &Texture, glow_gain: f32, glow: &mut bodies::DrawFabBodyGlow, moves: &[Option<Mat4f>], selected: &[fab_elec_core::CompRef]) -> u32 {
        if self.hidden { return 0; }
        self.bodies.draw_glow(cx, aux, glow_gain, glow, moves, selected)
    }

    pub fn draw_screens(&self, cx: &mut Cx3d, aux: &Texture, glow_gain: f32, draw: &mut bodies::DrawFabBodyScreen, moves: &[Option<Mat4f>], selected: &[fab_elec_core::CompRef]) -> u32 {
        if self.hidden { return 0; }
        self.bodies.draw_screens(cx, aux, glow_gain, draw, moves, selected)
    }

    /// Add each emitting lens's reflected light after drawing the lenses themselves.
    pub(super) fn draw_body_glow_pool(&mut self, cx: &mut Cx3d, aux: &Texture, lit: &Texture, glow_gain: f32, pool: &mut super::pool::DrawFabGlowPool, moves: &[Option<Mat4f>], pane: &mut bodies::PanePool) -> u32 {
        if self.hidden { return 0; }
        self.bodies.draw_glow_pool(cx, aux, lit, glow_gain, pool, moves, pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_elec_view::board::tiles::ART_TILE_BYTES;

    const TEST_ART_BUDGET_BYTES: usize = 64 * 1024 * 1024;

    /// Sync until the boards are placed and their bodies uploaded.
    fn settle(layer: &mut BoardLayer, cx: &mut Cx, state: &AppState) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let (mut moves, mut poses) = (Vec::new(), Vec::new());
        loop {
            layer.sync(cx, state);
            layer.moves_under(state, state.shared_poses(), &mut moves);
            if layer.pending.is_none() && !layer.bodies.building() && layer.board_poses(&moves, &mut poses).is_some() {
                return;
            }
            assert!(std::time::Instant::now() < deadline, "the boards and their bodies never settled");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// A board its case part holds keeps every body on it through edits: the scene an edit
    /// rebuilds (and its undo) poses each body by the held board's frame, never the free pose
    /// at the bed's corner, though no board pose changed. A part dragged on the board keeps its
    /// mesh, so each preview sample's scene re-poses the uploaded bodies once its boards are
    /// prepared, with no body build: the bodies follow the drag instead of jumping at the drop.
    #[test]
    fn held_board_bodies_stay_on_their_board_through_edits() {
        use fab_elec_mech::{scene::scene_of, Pose};
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let design = fab_elec_fixtures::ledctl();
        let first = Arc::new(scene_of(&design, &Pose::default()));
        // The case part's frame of the board: a quarter turn, lifted onto standoffs.
        let plate_from_frame = [[0.0, -1.0, 0.0, 120.0], [1.0, 0.0, 0.0, 90.0], [0.0, 0.0, 1.0, 4.0]];
        let mut state = AppState::default();
        state.plate.parts = vec![PlatePart {
            linked: Some(LinkedPlace { key: format!("board:{}", first.boards[0].name), plate_from_frame }),
            element: Some(ElementId(3)),
            ..Default::default()
        }];
        // The edit: one part dragged 2 mm along the board.
        let mut dragged = design.clone();
        let mut board = (**dragged.units[0].board.as_ref().expect("the fixture has a board")).clone();
        board.placed[0].at.x += fab_elec_core::mm(2.0);
        dragged.units[0].board = Some(Arc::new(board));
        let edited = Arc::new(scene_of(&dragged, &Pose::default()));
        let undone = Arc::new(scene_of(&design, &Pose::default()));
        assert_ne!(edited.hash, first.hash, "the drag changes the scene");
        let mut layer = BoardLayer::default();
        for (step, scene) in [("loaded", &first), ("edited", &edited), ("undone", &undone)] {
            state.elec_scene = Some(scene.clone());
            if step != "loaded" {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
                layer.sync(&mut cx, &state);
                while layer.pending.is_some() {
                    assert!(std::time::Instant::now() < deadline, "{step}: the boards were never prepared");
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    layer.sync(&mut cx, &state);
                }
                assert!(layer.bodies.shows(scene) && !layer.bodies.building(), "{step}: a moved part's bodies re-pose without a build");
            }
            settle(&mut layer, &mut cx, &state);
            assert_eq!(layer.drawn_parts(), &[ElementId(3)], "{step}: the case part stands in for its board");
            let placed = layer.bodies.placed();
            assert!(!placed.is_empty(), "{step}: the board's bodies are uploaded");
            for (board, body, fab_from_body) in placed {
                let want = Mat4f::mul(&rows_mat(&plate_from_frame), &bodies::xform_mat(&scene.boards[board].bodies[body].pose));
                let off = fab_from_body.v.iter().zip(&want.v).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(off < 1e-3, "{step}: body {body} of board {board} sits where the case holds its board (off by {off})");
            }
        }
        // A drag's gesture poses (`AppState::elec_poses`) move the uploaded bodies at once, with
        // no scene, no preparation and no build (EBOARD 11-16: the 3D bodies froze mid-drag).
        let unit = design.units[0].id;
        let item = design.units[0].board.as_ref().expect("the fixture has a board").placed[0].id;
        state.elec_poses = Some(fab_elec_mech::scene::moved_poses(&design, &[(unit, item, fab_elec_core::P::mm(2.0, 0.0))]).into());
        assert!(layer.sync(&mut cx, &state), "gesture poses: new poses change the drawing");
        assert!(layer.pending.is_none() && !layer.bodies.building(), "gesture poses: no preparation or body build is asked for");
        let sits_as = |layer: &BoardLayer, scene: &ElecScene, step: &str| {
            for (board, body, fab_from_body) in layer.bodies.placed() {
                let want = Mat4f::mul(&rows_mat(&plate_from_frame), &bodies::xform_mat(&scene.boards[board].bodies[body].pose));
                let off = fab_from_body.v.iter().zip(&want.v).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(off < 1e-3, "{step}: body {body} of board {board} sits where that scene has it (off by {off})");
            }
        };
        sits_as(&layer, &edited, "gesture poses");
        // The engine snaps the drop 0.5 mm past the drag's last step.
        let mut snapped = design.clone();
        let mut board = (**snapped.units[0].board.as_ref().expect("the fixture has a board")).clone();
        board.placed[0].at.x += fab_elec_core::mm(2.5);
        snapped.units[0].board = Some(Arc::new(board));
        let prepare = |layer: &mut BoardLayer, cx: &mut Cx, state: &AppState, step: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
            layer.sync(cx, state);
            while layer.pending.is_some() {
                assert!(std::time::Instant::now() < deadline, "{step}: the boards were never prepared");
                std::thread::sleep(std::time::Duration::from_millis(2));
                layer.sync(cx, state);
            }
            settle(layer, cx, state);
        };
        // A preview sample's scene landing while the drag is live keeps the drag's poses.
        state.elec_previewing = true;
        state.elec_scene_committed = false;
        state.elec_scene = Some(Arc::new(scene_of(&snapped, &Pose::default())));
        prepare(&mut layer, &mut cx, &state, "a preview scene mid-drag");
        sits_as(&layer, &edited, "a preview scene mid-drag keeps the drag's poses");
        // The first committed scene after the drag poses its bodies where the engine put them, though
        // that is not the drag's last pose: the drag's poses go (a stale one would stay forever).
        state.elec_previewing = false;
        state.elec_scene_committed = true;
        let committed = Arc::new(scene_of(&snapped, &Pose::default()));
        state.elec_scene = Some(committed.clone());
        prepare(&mut layer, &mut cx, &state, "the committed scene");
        sits_as(&layer, &committed, "the committed scene after a snapped drop");
        // A scene rebuild binds the case part to a new element with no plate edit (Design's or
        // Mechanics' scene installed): the board stands in for that element and follows its pose,
        // never the gone one's (11-18: the part's own mesh drew through the board, and a pull in
        // Mechanics carried it off without the board).
        state.plate.parts[0].element = Some(ElementId(9));
        state.scene_revision += 1;
        layer.sync(&mut cx, &state);
        assert_eq!(layer.drawn_parts(), &[ElementId(9)], "the rebuilt scene's part stands in for its board");
        let rest: Vec<_> = layer.bodies.instance_poses().collect();
        let builds = layer.bodies.build_requests();
        let mut moves = Vec::new();
        layer.moves_under(&state, state.shared_poses(), &mut moves);
        assert_eq!(moves[0], None, "a holder at rest needs no draw-time move");
        state.scene_state.set_lift(Some((Arc::new([ElementId(9)].into_iter().collect()), 0.005)));
        // Plate X and board X differ by a quarter turn, so a sideways nudge detects a
        // move composed on the wrong side of the rest placement.
        let mut slide = crate::model::state::POSE_IDENTITY;
        slide[0][3] = 0.003;
        for (step, nudge, x_mm) in [("lift", None, 0.0), ("lift and slide", Some((ElementId(9), slide)), 3.0)] {
            state.scene_state.set_nudge(nudge);
            assert!(!layer.sync(&mut cx, &state), "{step}: a holder move leaves shared board content unchanged");
            layer.moves_under(&state, state.shared_poses(), &mut moves);
            let lifted = Mat4f::mul(&Mat4f::translation(vec3(x_mm, 0.0, 5.0)), &rows_mat(&plate_from_frame));
            for (board, body, fab_from_body) in layer.bodies.placed() {
                let want = Mat4f::mul(&lifted, &bodies::xform_mat(&committed.boards[board].bodies[body].pose));
                let drawn = moved_pose(fab_from_body, moves[board]);
                let off = drawn.v.iter().zip(&want.v).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
                assert!(off < 1e-3, "{step}: body {body} of board {board} rides 5 mm up and {x_mm} mm along plate X with the new element (off by {off})");
            }
            assert!(layer.bodies.instance_poses().eq(rest.iter().copied()), "{step}: the holder move preserves every stored instance matrix");
            assert_eq!(layer.bodies.build_requests(), builds, "{step}: the holder move requests no body build");
        }

        // Opening and closing an archived job leaves the plate, board scene and holder pose alone.
        use crate::print::{apply, jobs::{CommittedJob, JobMeta, JobState}, PrintAction};
        let grips = layer.port_tips(false, &moves);
        let &(button, tip) = grips.first().expect("the board has a push button");
        let ray = crate::model::query::Ray::new(tip + vec3(0.0, 0.0, 0.02), vec3(0.0, 0.0, -1.0));
        state.print.follow.jobs.push(CommittedJob {
            meta: Arc::new(JobMeta { id: "archived".into(), ..Default::default() }),
            state: JobState::Finished,
            ..Default::default()
        });
        state.print.follow.monitor = true;
        for (action, visible) in [(PrintAction::PickJob(Some("archived".into())), false), (PrintAction::CloseJob, true)] {
            apply(&mut state.print, &action);
            let changed = layer.sync(&mut cx, &state);
            assert_eq!(layer.drawable(), visible, "{action:?}: the lit pass gates the PCB and bodies");
            assert!(changed, "{action:?}: the viewport must record its passes again");
            let event = DrawEvent::default();
            let mut draw = CxDraw::new(&mut cx, &event);
            assert_eq!(layer.select(&mut draw, &SceneState3D::default(), &moves), visible, "{action:?}: the auxiliary pass gates the PCB and bodies");
            assert_eq!(layer.comp_at(&ray, &moves).map(|hit| hit.0), visible.then_some(button), "{action:?}: the ray picks only a visible button");
            assert_eq!(layer.port_tips(false, &moves), if visible { grips.as_slice() } else { &[] }, "{action:?}: only visible controls offer grips");
            let mut casters = layer.casters.borrow_mut();
            let revision = casters.revision();
            casters.set_hidden(!visible);
            assert_eq!(casters.revision(), revision, "{action:?}: the shadow casters already have the requested visibility");
        }
    }

    #[test]
    fn selected_faces_use_shared_art_without_sampling_a_neighbour() {
        let bounds = Aabb { min: vec3(0.0, 0.0, 0.0), max: vec3(1.0, 1.0, 0.1) };
        let board = art_box(&bounds);
        let roots = TileId::covering(BoardArt::<u32>::base_lod(board), board);
        let mut art = BoardArt::<u32>::new(board, TEST_ART_BUDGET_BYTES);
        let mut wanted = [Vec::new(), Vec::new()];
        want_art(&mut art, &roots, &wanted, true);
        assert_eq!(art.missing().len(), 2 * roots.len(), "both sides start with their base coverage wanted");
        for (plane, tile) in art.missing() {
            art.put(plane, tile, 1, 0);
        }

        let view = TileView {
            bounds,
            eye_mm: [0.5, 0.5, 1.0],
            clip_from_board: Mat4f::identity(),
            pixels_per_m: 1e6,
            perspective: false,
        };
        for &root in &roots {
            view.visit(root, 0.1, &mut wanted[0]);
        }
        let fine = wanted[0][0];
        assert!(fine.lod < roots[0].lod, "the viewer selects finer face tiles");
        let neighbour = TileId { x: fine.x + 1, ..fine };
        assert!(wanted[0].contains(&neighbour), "the adjacent tile is also in view");
        want_art(&mut art, &roots, &wanted, true);
        let missing = art.missing();
        assert_eq!(missing.len(), wanted[0].len(), "the next host frame requests the recorded fine selection");
        assert!(missing.iter().all(|&(plane, _)| plane == surface_plane(false)), "only the selected side needs fine art");

        art.put(surface_plane(false), neighbour, 2, 0);
        let mut cover = Vec::new();
        let fallback = cover_art(&art, false, fine, &mut cover).expect("base coverage while fine art is pending");
        assert!(roots.contains(&fallback.tile), "a resident neighbour must not replace the tile's ancestor");
        assert_eq!(fallback.rect_nm, fallback.tile.rect_nm(), "fallback UVs use the ancestor rectangle");
        for (plane, tile) in art.missing() {
            art.put(plane, tile, 3, 0);
        }
        let resident = cover_art(&art, false, fine, &mut cover).expect("completed fine art");
        assert_eq!((resident.tile, resident.texture), (fine, 3), "completed work replaces the ancestor for this face");
        assert_eq!(resident.rect_nm, fine.rect_nm(), "fine UVs use the fine rectangle");
        assert!(roots.contains(&cover_art(&art, true, fine, &mut cover).expect("bottom base coverage").tile), "top completion does not replace bottom art");

        let fine_count = wanted[0].len();
        art.set_budget(art.resident() * ART_TILE_BYTES);
        wanted[0].clear();
        want_art(&mut art, &roots, &wanted, true);
        assert!(art.evict().is_empty(), "unwanted art stays while the resident bytes fit the budget");
        assert!(art.tile(surface_plane(false), fine).is_some(), "idle fine art is still reusable");
        art.set_budget(2 * roots.len() * ART_TILE_BYTES);
        assert_eq!(art.evict().len(), fine_count, "only over-budget unwanted art is evicted");
        for bottom in [false, true] {
            assert!(roots.iter().all(|&tile| art.tile(surface_plane(bottom), tile).is_some()), "both sides' base levels stay resident");
        }
        assert!(roots.contains(&cover_art(&art, false, fine, &mut cover).expect("base survives eviction").tile), "an evicted fine tile still has coverage");
    }
}

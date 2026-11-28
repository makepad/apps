//! `FabViewport` — the plate view's realtime 3D viewport. **The renderer is
//! `libs/render`**: its PBR lane, its cascaded shadow maps, its analytic sky
//! and sun. Solid (clay) is an overlay pass on top of that lit image, never a
//! second shading pipeline.
//!
//! ## The pass chain (deepest first, which is the order they execute)
//!
//! ```text
//! aux  RGBA32F + D32   our G-buffer: view depth, octahedral view normal,
//!                      signed element id. Also the DEPTH PREPASS the lit
//!                      pass reuses (LessEqual, no writes). It is the
//!                      authority on visibility and section clipping — the
//!                      lit pass is told the same things through the two
//!                      generic hooks in `libs/render`.
//! ssao CORTAO          per-view prepass → main → filter → temporal resolve,
//!                      over aux's depth and normals at full resolution in
//!                      Realtime. StillAa samples skip the temporal resolve.
//!                      Meshes take it on their ambient fill through
//!                      the renderer's hook, the composite on the direct
//!                      light by the model lanes' `ao_direct` rule, and on
//!                      both for the surfaces it lights (tiles, beads).
//! lit  RGBA16F (HDR) / BGRA8 + that D32: PBR materials, sky,
//!                      sun and CSM shadows (textured diffuse for Solid).
//! comp BGRA8           the composite quad (Solid's clay and cavity,
//!                      outlines, hover, x-ray, section caps, background),
//!                      then the infinite grid, occluded against `aux`.
//! still_aa             averages the composite's display values while still.
//! glow RGBA16F         live lenses and their light on aux's visible surfaces.
//! bloom RGBA16F        the glow layer's downsample → upsample → adapt chain.
//! window               `draw_bg` blits the average or comp, then the glow
//!                      quad adds lens light and halo as a display difference.
//! ```
//!
//! ## The two hooks in `libs/render`, both off by default
//!
//! 1. **Per-element lookup** — `elem_map` + `elem_ctl` on the static-mesh
//!    shader. The element index rides the free `ao_uv` vertex lane; one RGBA
//!    texel per element carries `(state, offset.xyz)`. Hide / isolate /
//!    select / a dragged part's preview therefore cost one small upload per
//!    `SceneState::revision`, and zero geometry re-uploads.
//!    Layout: `viewport::elements`.
//! 2. **Clip planes** — `clip_ctl` + `clip0..clip5`, world-space half-spaces
//!    the pixel stage discards against. Section planes and the section box
//!    both compile down to these. A resin layer's cut is one more, `clip_cut`,
//!    which a new layer moves in the retained passes without recording them;
//!    the lit pass's meshes take it as the renderer's clip plane
//!    (`Renderer::set_clip_plane`, moved with `move_clip_plane`).
//!
//! Input goes out through `api::ViewportInput` to the navigator and then the
//! tool set. Everything drawn *over* the viewport (gizmos, tool overlays) is a
//! separate widget stacked by the plate view (`ui/stage.rs`).

pub mod board;
pub mod bodies;
pub mod solder;
mod pool;
pub mod board_scene;
pub mod glass;
pub mod board_geom;
mod casters;
mod dsl;
pub mod elements;
pub mod force;
pub mod pack;
pub mod paint;
mod picture;
mod part_glow;
mod product_scene;
mod press;
mod screen;
pub mod print;
pub mod sdf;
pub mod see_through;
mod stage;

use crate::api::*;
use crate::nav::Navigator;
use crate::tools::ToolSet;
use makepad_render::cortao::shaders::DrawCortaoPrepass;
use makepad_render::cortao::{
    CortaoCamera, CortaoConfig, CortaoDepth, CortaoInput, CortaoOutput, CortaoPass, CortaoTable,
    CortaoTier,
};
use makepad_render::{
    DrawSceneShadow, DrawSceneSkinned, DrawSceneSky, DrawSceneSkyAnalytic, GpuLightmapMode,
    Renderer, SceneDraws, StillAa, DEFAULT_CSM_CONFIG, DEFAULT_SHADOW_BUDGET,
};
use makepad_render_graph::BloomPass;
use fab_sdf_tiles::draw::TileLight;
use fab_elec_view::{live::sim::SimLive, ElecLive};
use makepad_widgets_core::*;
use makepad_widgets_core::makepad_platform::event::DigitId;
use makepad_widgets_core::makepad_platform::retained_instances::RetainedInstances;
use std::{cell::RefCell, rc::Rc, sync::Arc};

use elements::{ElementLut, STRIDE};

pub mod scene_gpu;
use scene_gpu::{SceneGpu, SceneSync};

#[derive(Clone, Debug)]
pub(crate) struct ViewportContextMenu {
    pub at: DVec2,
    /// The Sliced view's branch under the click (its tree's place in `PrintState::trees`, its
    /// lowest disc): picked, and the menu offers to delete it.
    pub branch: Option<(usize, u32)>,
    /// The no-trees area under the click (`instance/solid`): picked, and the menu offers to
    /// delete it.
    pub area: Option<String>,
    /// The part under the click (its object): what the menu's print settings volume covers.
    pub part: Option<String>,
    /// The named feature of it under the click (`instance/solid`): what a volume can follow.
    pub feature: Option<String>,
}

/// Albedo lift of a 10 mm line on the plate.
const PLATE_LINE_MINOR: f32 = 0.22;
/// Albedo lift of a 50 mm line on the plate.
const PLATE_LINE_MAJOR: f32 = 0.5;
/// How near the surface the pass drew a ghost may lie and still be that surface (metres): a joint
/// check's ghost on its own part (dragged to where it stops, or a wheel half a turn round) is one
/// body as two meshes, each within its chord. There the part shows, so the two never take turns
/// over a pixel.
const GHOST_TIE: f32 = 0.0003;
/// The display density from which See-Through marches half the pixels each way while the view
/// moves (the march is the composite's dearest part). A still view marches them all: half of
/// them breaks a shell's one-pixel edge line into dashes.
const SEE_HALF_DPI: f64 = 2.0;
/// A recorded scene that still waits for the renderer (`items_ready`) after this many seconds
/// says once in the log what it waits for.
const ITEMS_WAIT_SAY: f64 = 5.0;

/// How near on screen (points) a pointer takes a live control's grip: a 28 point target at any
/// zoom (`FabViewport::pick_control`).
const GRAB_PT: f64 = 14.0;

/// A live control's grip as a pane shows it: where a press near it on screen takes it
/// (`FabViewport::pick_control`), and what the view's description reports of it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct ControlGrip {
    comp: fab_elec_core::CompRef,
    /// A knob a drag turns, else a button a press holds.
    knob: bool,
    /// Plate metres.
    at: Vec3f,
}

/// Registered before the widget modules that place it (`main.rs` order).
/// `libs/render`'s own shaders have to exist before ours can name them.
pub fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
    makepad_render::script_mod(vm);
    makepad_raytrace::script_mod(vm);
    fab_sdf_tiles::script_mod(vm);
    print::script_mod(vm);
    force::script_mod(vm);
    fab_elec_view::live::script_mod(vm);
    fab_elec_view::live::strip::script_mod(vm);
    // The firmware's steps, before the transport and the Firmware pane that hold them.
    fab_elec_view::live::fw_steps::script_mod(vm);
    fab_elec_view::live::overview::script_mod(vm);
    fab_elec_view::live::transport::script_mod(vm);
    // The board canvas and its art raster, after the live-row functions their shaders use and
    // before the viewport and the Electronics workspace place them.
    fab_elec_view::board::script_mod(vm);
    // The circuit explorer's canvas shaders and widget, for the Electronics workspace's Circuit tab.
    fab_elec_view::canvas::draw::script_mod(vm);
    fab_elec_view::explorer::view::script_mod(vm);
    // The bench's trace segments, then its instruments, for the Electronics workspace's Bench tab.
    fab_elec_view::trace::script_mod(vm);
    fab_elec_view::instruments::script_mod(vm);
    fab_elec_view::live::controls_strip::script_mod(vm);
    // The Electronics workspace's library (in Fab's Library column) and its Firmware pane.
    fab_elec_view::library::script_mod(vm);
    fab_elec_view::fw_rows::script_mod(vm);
    fab_elec_view::firmware::script_mod(vm);
    dsl::cortao_weights::script_mod(vm);
    bodies::script_mod(vm);
    pool::script_mod(vm);
    board::script_mod(vm);
    casters::script_mod(vm);
    glass::script_mod(vm);
    picture::script_mod(vm);
    dsl::script_mod(vm)
}

// ===========================================================================
// Vertex formats
// ===========================================================================

/// The aux pass draws the Fab stream verbatim — `fab::model::Vertex`,
/// stride 12, no conversion (`batch.rs`'s promise).
#[repr(C)]
#[derive(Clone, Script, ScriptHook)]
pub struct FabVertex {
    #[live]
    pub pos: Vec3f,
    #[live]
    pub element: f32,
    #[live]
    pub normal: Vec3f,
    #[live]
    pub pad: f32,
    #[live]
    pub uv: Vec2f,
    #[live]
    pub pad2: Vec2f,
}

// ---------------------------------------------------------------------------
// std140 vs repr(C): pinned at COMPILE time, because the alternative is a
// panic before the first frame.
//
// `ScriptPod::script_pod` (`platform/script/src/traits.rs:376`) walks both
// layouts side by side and asserts they agree field for field: Rust `repr(C)`
// aligns a `Vec3f` to 4, std140 aligns it to 16, and the struct's std140 size
// rounds up to 16. A `Vec3f` at Rust offset 36 therefore lands at shader
// offset 48 and the app dies at startup with "Rust POD field offset mismatch".
// Nobody should have to launch a GPU binary to find that out.
//
// `pod_layout_end` below is that same walk, in a `const fn`, over the field
// table; `offset_of!` ties the table to the actual struct. Change a field and
// this stops compiling.
// ---------------------------------------------------------------------------

/// `[rust_align, rust_size, std140_align, std140_size]` per field type.
const F_F32: [usize; 4] = [4, 4, 4, 4];
const F_VEC2: [usize; 4] = [4, 8, 8, 8];
const F_VEC3: [usize; 4] = [4, 12, 16, 12];

const fn align_up(v: usize, a: usize) -> usize {
    v.div_ceil(a) * a
}

/// Both layouts, walked together; panics (at compile time, in a `const`
/// context) the moment they disagree. Returns the shared struct size.
const fn pod_layout_end(fields: &[[usize; 4]]) -> usize {
    let mut i = 0;
    let (mut rust_off, mut shader_off, mut shader_align) = (0usize, 0usize, 4usize);
    while i < fields.len() {
        let f = fields[i];
        rust_off = align_up(rust_off, f[0]);
        shader_off = align_up(shader_off, f[2]);
        assert!(
            rust_off == shader_off,
            "fab vertex format: std140 and repr(C) disagree on a field offset — add explicit padding"
        );
        rust_off += f[1];
        shader_off += f[3];
        if f[2] > shader_align {
            shader_align = f[2];
        }
        i += 1;
    }
    assert!(
        align_up(rust_off, 4) == align_up(shader_off, shader_align),
        "fab vertex format: std140 and repr(C) disagree on the struct size — pad the tail"
    );
    align_up(shader_off, shader_align)
}

const _: () = {
    use std::mem::{offset_of, size_of};

    // FabVertex — pos, element, normal, pad, uv, pad2.
    assert!(pod_layout_end(&[F_VEC3, F_F32, F_VEC3, F_F32, F_VEC2, F_VEC2]) == VERTEX_STRIDE * 4);
    assert!(size_of::<FabVertex>() == VERTEX_STRIDE * 4);
    assert!(offset_of!(FabVertex, pos) == 0);
    assert!(offset_of!(FabVertex, element) == 12);
    assert!(offset_of!(FabVertex, normal) == 16);
    assert!(offset_of!(FabVertex, pad) == 28);
    assert!(offset_of!(FabVertex, uv) == 32);
    assert!(offset_of!(FabVertex, pad2) == 40);
};

pub(crate) fn fab_vertex_pod(vm: &mut ScriptVm) -> ScriptValue {
    let v = FabVertex::script_pod(vm).expect("FabVertex pod");
    vm.heap_mut().pod_type_name_set(v, id_lut!(FabVertex));
    v.into()
}

/// Only fixes the vertex format for the shader; real geometry is bound per
/// draw call.
pub(crate) fn fab_placeholder_geom(vm: &mut ScriptVm) -> ScriptValue {
    let id = vm.cx_mut().shared_geometry(id!(FabGeom), |cx| {
        let g = Geometry::new(cx);
        let mut v = Vec::new();
        for p in [[0.0f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]] {
            v.extend_from_slice(&[p[0], p[1], p[2], 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        }
        g.update(cx, vec![0, 1, 2], v);
        g
    });
    Geometry::new_borrowed(id).into_script_handle(vm)
}

// ===========================================================================
// Draw structs
// ===========================================================================

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawFabCortaoPrepass {
    #[deref]
    draw_super: DrawCortaoPrepass,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabAux {
    #[deref]
    draw_vars: DrawVars,
    #[live]
    transform: Mat4f,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabGrid {
    #[deref]
    draw_vars: DrawVars,
    #[live]
    transform: Mat4f,
    #[live(300.0)]
    fade_far: f32,
    /// Orthographic: the distance the view's height would be seen at in perspective, which sets
    /// the line spacing everywhere; 0 in perspective (each line's own distance).
    #[live(0.0)]
    ortho_reach: f32,
}

/// The build volume's edges in the composite (see `dsl.rs`).
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabLines {
    #[deref]
    draw_vars: DrawVars,
    #[live]
    color: Vec4f,
}

/// The picks' outlines in the composite (see `dsl.rs`).
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabPickLines {
    #[deref]
    draw_vars: DrawVars,
    #[live]
    color: Vec4f,
    #[live]
    halo: Vec4f,
}

/// A resin printer's layer image on the parts' cut (see `dsl.rs`).
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabResinLayer {
    #[deref]
    draw_vars: DrawVars,
    /// A wholly lit pixel's colour, and the screen's around the lit ones (premultiplied by
    /// its alpha in the shader).
    #[live]
    lit: Vec4f,
    #[live]
    screen: Vec4f,
    /// The layer's top above the bed (world): the quad lies on the bed, lifted to its cut.
    #[live]
    lift: f32,
}

/// A part drawn see-through over the composite (see `dsl.rs`, `see_through.rs`): its grid box,
/// marched front to back.
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabSeeThrough {
    #[deref]
    draw_vars: DrawVars,
    /// World (metres) from the grid's lattice, and back: three rows of an affine map.
    #[live]
    world_from_lattice: Mat4f,
    #[live]
    lat_x: Vec4f,
    #[live]
    lat_y: Vec4f,
    #[live]
    lat_z: Vec4f,
    /// Bricks per axis, and the first one's texel in the index.
    #[live]
    bricks: Vec4f,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabComposite {
    #[deref]
    draw_super: DrawQuad,
}

/// The rebuilding sweep, drawn in the window pass over the retained composite.
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabRebuild {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    mirror_x: f32,
}

/// The glow layer's display difference over the retained picture, in the window pass.
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabGlowOver {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    mirror_x: f32,
}

/// See-Through's half-resolution march taken up into the composite (see `dsl.rs`).
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabSeeUp {
    #[deref]
    draw_super: DrawQuad,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabSceneTexture {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    mirror_x: f32,
    #[live(1.0)]
    scene_ready: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabCrosshair {
    #[deref]
    draw_super: DrawQuad,
}

/// The ring round the live control under the pointer: the target a press takes it in.
#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabControlRing {
    #[deref]
    draw_super: DrawQuad,
}

impl DrawFabSceneTexture {
    pub fn set_scene_texture(&mut self, texture: &Texture) {
        self.draw_super.draw_vars.set_texture(0, texture);
    }
}

trait DrawGeometry {
    fn vars(&mut self) -> &mut DrawVars;
    fn draw_geometry(&mut self, cx: &mut CxDraw, geometry_id: GeometryId) {
        let vars = self.vars();
        vars.geometry_id = Some(geometry_id);
        if vars.can_instance() {
            let area = vars.area;
            let new_area = cx.add_instance(vars);
            self.vars().area = cx.update_area_refs(area, new_area);
        }
    }
}

impl DrawGeometry for DrawFabSeeThrough {
    fn vars(&mut self) -> &mut DrawVars {
        &mut self.draw_vars
    }
}

impl DrawGeometry for DrawFabAux {
    fn vars(&mut self) -> &mut DrawVars {
        &mut self.draw_vars
    }
}
impl DrawGeometry for DrawFabGrid {
    fn vars(&mut self) -> &mut DrawVars {
        &mut self.draw_vars
    }
}
impl DrawGeometry for DrawFabLines {
    fn vars(&mut self) -> &mut DrawVars {
        &mut self.draw_vars
    }
}
impl DrawGeometry for DrawFabPickLines {
    fn vars(&mut self) -> &mut DrawVars {
        &mut self.draw_vars
    }
}
impl DrawGeometry for DrawFabResinLayer {
    fn vars(&mut self) -> &mut DrawVars {
        &mut self.draw_vars
    }
}

/// `texture` in the slot of the shader's texture `name` (the slots are the shader's order, not
/// the declaration's).
fn set_texture_named(vars: &mut DrawVars, cx: &Cx, name: LiveId, texture: &Texture) {
    let slot = vars.draw_shader_id.and_then(|shader| cx.draw_shaders[shader.index].mapping.textures.iter().position(|t| t.id == name));
    if let Some(slot) = slot {
        vars.set_texture(slot, texture);
    }
}

/// The plane the shown layer cuts the parts at: everything above its top goes (`n · p + d >= 0`
/// keeps, world metres). A resin printer's shown layer always cuts. A slice's cuts the parts drawn
/// among its toolpaths (inserts, and the model's parts while they show) when Cut Parts at the
/// Layer is on (`Playback::cut`) and the slider's layer leads; at the last layer the print shows
/// whole and nothing is cut.
fn layer_cut(state: &AppState) -> Option<[f32; 4]> {
    let top = match state.resin_shown() {
        Some(show) => show.top_of(show.image.as_ref()?.layer),
        None => {
            let preview = state.slice_shown().filter(|_| state.print.play.cut)?;
            let layer = state.print.play.layer_hi;
            if !print::layer_leads(layer, preview) {
                return None;
            }
            preview.layers.iter().find(|l| l.layer == layer)?.z as f64
        }
    };
    Some([0.0, 0.0, -1.0, crate::tools::transform::mm_to_world([0.0, 0.0, top]).z])
}

/// Whether the composite paints the inside a cut leaves open as the cut's flat face. A layer's
/// cut (a resin layer, or a slice's with Cut Parts at the Layer) always does, so a cut part reads
/// as solid rather than as an empty shell; a section's planes do as the section asks.
fn caps_shown(state: &AppState, planes: &[[f32; 4]]) -> bool {
    layer_cut(state).is_some() || (!planes.is_empty() && (state.scene_state.section.caps || state.test_cut_shown()))
}

/// The resin cut as the clipping draws take it: in Fab's frame and in the renderer's (none: a
/// zero plane, which clips nothing).
fn cut_planes(cut: Option<[f32; 4]>) -> ([f32; 4], [f32; 4]) {
    match cut {
        Some(c) => (c, sdf::planes_in_world(&[c], &render_from_fab())[0]),
        None => ([0.0; 4], [0.0; 4]),
    }
}

/// The quad a resin layer's image lies on: its pixels' box on the bed, the texture's rows from
/// the box's lowest Y (`DrawFabResinLayer::lift` takes it to the layer's top).
fn resin_quad(cx: &mut Cx, rect: [u32; 4], pitch: [f64; 2]) -> Geometry {
    let [x0, y0, x1, y1] = rect.map(|v| v as f64);
    let mut v = Vec::with_capacity(4 * VERTEX_STRIDE);
    for (x, y, u, w) in [(x0, y0, 0.0, 0.0), (x1, y0, 1.0, 0.0), (x1, y1, 1.0, 1.0), (x0, y1, 0.0, 1.0)] {
        let p = crate::tools::transform::mm_to_world([x * pitch[0], y * pitch[1], 0.0]);
        v.extend_from_slice(&[p.x, p.y, p.z, 0.0, 0.0, 0.0, 1.0, 0.0, u, w, 0.0, 0.0]);
    }
    let g = Geometry::new(cx);
    g.update(cx, vec![0, 1, 2, 0, 2, 3], v);
    g
}

/// The build volume the view outlines: the bed's rectangle (mm) and the printer's height.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Volume {
    min: [f64; 2],
    max: [f64; 2],
    height: f64,
}

impl Volume {
    /// None in Mechanics (the piece moves free of the printer) or without a printer height.
    fn of(state: &AppState) -> Option<Volume> {
        let p = &state.plate;
        (!state.ui.in_mode(PlateMode::Mechanics) && p.max_height > 0.0).then_some(Volume { min: p.bed_min, max: p.bed_max, height: p.max_height })
    }

    /// Its four uprights and its top's four sides, a quad each: two vertices at each end
    /// (`DrawFabLines`).
    fn lines(&self, cx: &mut Cx) -> Geometry {
        let (a, b, h) = (self.min, self.max, self.height);
        let corners = [[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]]];
        let at = |c: [f64; 2], z: f64| crate::tools::transform::mm_to_world([c[0], c[1], z]);
        let segments: Vec<(Vec3f, Vec3f)> = (0..4).flat_map(|i| [(at(corners[i], 0.0), at(corners[i], h)), (at(corners[i], h), at(corners[(i + 1) % 4], h))]).collect();
        line_geometry(cx, segments.into_iter())
    }
}

/// Segments (world) as `DrawFabLines` quads: two vertices at each end.
fn line_geometry(cx: &mut Cx, segments: impl ExactSizeIterator<Item = (Vec3f, Vec3f)>) -> Geometry {
    let (index, v) = line_buffers(segments);
    let g = Geometry::new(cx);
    g.update(cx, index, v);
    g
}

/// The indices and vertices of [`line_geometry`].
fn line_buffers(segments: impl ExactSizeIterator<Item = (Vec3f, Vec3f)>) -> (Vec<u32>, Vec<f32>) {
    let mut v = Vec::with_capacity(segments.len() * 4 * VERTEX_STRIDE);
    let mut index = Vec::with_capacity(segments.len() * 6);
    for (k, (p, q)) in segments.enumerate() {
        for (end, this, other, side) in [(0.0, p, q, -1.0), (0.0, p, q, 1.0), (1.0, q, p, 1.0), (1.0, q, p, -1.0)] {
            v.extend_from_slice(&[this.x, this.y, this.z, side, other.x, other.y, other.z, 0.0, end, 0.0, 0.0, 0.0]);
        }
        let o = (k * 4) as u32;
        index.extend_from_slice(&[o, o + 1, o + 2, o, o + 2, o + 3]);
    }
    (index, v)
}

/// Polylines (world) as `DrawFabPickLines` quads: [`line_buffers`]' with each vertex's way along
/// its line (metres) in `uv.y`.
fn pick_line_buffers(lines: &[Vec<Vec3f>]) -> (Vec<u32>, Vec<f32>) {
    let count: usize = lines.iter().map(|l| l.len().saturating_sub(1)).sum();
    let mut v = Vec::with_capacity(count * 4 * VERTEX_STRIDE);
    let mut index = Vec::with_capacity(count * 6);
    let mut k = 0u32;
    for line in lines {
        let mut along = 0.0f32;
        for w in line.windows(2) {
            let (p, q) = (w[0], w[1]);
            let next = along + (q - p).length();
            for (end, this, other, side, s) in [(0.0, p, q, -1.0, along), (0.0, p, q, 1.0, along), (1.0, q, p, 1.0, next), (1.0, q, p, -1.0, next)] {
                v.extend_from_slice(&[this.x, this.y, this.z, side, other.x, other.y, other.z, 0.0, end, s, 0.0, 0.0]);
            }
            let o = k * 4;
            index.extend_from_slice(&[o, o + 1, o + 2, o, o + 2, o + 3]);
            k += 1;
            along = next;
        }
    }
    (index, v)
}

/// The picks' outlines in the world, by group: what a click would take (in the pane it was
/// asked in), the picks, the one whose chat chip is under the pointer. O(their points) a frame.
fn pick_lines(state: &AppState, pane: PaneId) -> [Vec<Vec<Vec3f>>; 3] {
    use crate::tools::picks::on_plate;
    let picks = &state.picks;
    let world = |object: &str, lines: &[Vec<[f64; 3]>], out: &mut Vec<Vec<Vec3f>>| {
        for line in lines {
            let pts: Option<Vec<Vec3f>> = line.iter().map(|p| on_plate(state, object, *p).map(crate::tools::transform::mm_to_world)).collect();
            if let Some(pts) = pts.filter(|p| p.len() > 1) {
                out.push(pts);
            }
        }
    };
    let mut out: [Vec<Vec<Vec3f>>; 3] = Default::default();
    if let (Some(under), None, None) = (&picks.under, &picks.other, &picks.menu) {
        if let Some((object, lines)) = under.outline.as_ref().filter(|_| under.view.0 == *state.view.camera(pane)) {
            world(object, lines, &mut out[0]);
        }
    }
    for pick in picks.list.iter() {
        let lit = picks.lit.as_deref() == Some(pick.id.as_str());
        world(&pick.object, &pick.outline, &mut out[if lit { 2 } else { 1 }]);
    }
    out
}

/// The unit cube, each face with its outward normal (`DrawFabSeeThrough` scales it to a grid).
fn unit_cube(cx: &mut Cx) -> Geometry {
    let g = Geometry::new(cx);
    let (mut index, mut v) = (Vec::with_capacity(36), Vec::with_capacity(24 * VERTEX_STRIDE));
    for axis in 0..3 {
        for side in [0.0f32, 1.0] {
            let mut n = [0.0f32; 3];
            n[axis] = side * 2.0 - 1.0;
            let (u, w) = ((axis + 1) % 3, (axis + 2) % 3);
            let base = (v.len() / VERTEX_STRIDE) as u32;
            for (a, b) in [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)] {
                let mut p = [0.0f32; 3];
                p[axis] = side;
                p[u] = a;
                p[w] = b;
                v.extend_from_slice(&[p[0], p[1], p[2], 0.0, n[0], n[1], n[2], 0.0, a, b, 0.0, 0.0]);
            }
            index.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
        }
    }
    g.update(cx, index, v);
    g
}

/// A ground quad big enough to read as infinite; the shader fades it.
fn ground_quad(cx: &mut Cx, half: f32) -> Geometry {
    let g = Geometry::new(cx);
    let mut v = Vec::with_capacity(4 * VERTEX_STRIDE);
    for (x, y) in [(-half, -half), (half, -half), (half, half), (-half, half)] {
        v.extend_from_slice(&[x, y, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
    }
    g.update(cx, vec![0, 1, 2, 0, 2, 3], v);
    g
}

/// rig: the words of the reference rig's mode file (`FAB_XR_FAR_MODE_FILE`), empty without one.
fn rig_mode() -> String {
    std::env::var_os("FAB_XR_FAR_MODE_FILE").and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default()
}

/// rig: the third of the view `crop<x><y>` draws (see `scene_states`).
fn rig_crop() -> Option<(u32, u32)> {
    rig_mode().split_whitespace().find_map(|w| {
        let d: Vec<u32> = w.strip_prefix("crop")?.chars().map(|c| c.to_digit(3)).collect::<Option<_>>()?;
        (d.len() == 2).then(|| (d[0], d[1]))
    })
}

/// rig: the projection of the third of the view `crop<x><y>` draws (x, y from the top left, 0..2)
/// over the whole viewport: the same rays, pixels a third the size (the 9x reference).
fn rig_crop_projection(projection: Mat4f, (x, y): (u32, u32)) -> Mat4f {
    let (cx, cy) = (-1.0 + (2.0 * x as f32 + 1.0) / 3.0, 1.0 - (2.0 * y as f32 + 1.0) / 3.0);
    let c = Mat4f { v: [3.0, 0.0, 0.0, 0.0, 0.0, 3.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -3.0 * cx, -3.0 * cy, 0.0, 1.0] };
    Mat4f::mul(&c, &projection)
}

/// rig: the display scale the view's point-sized marks (the layer's outline, the volume's lines,
/// the nozzle's ring, the selection's edge) are drawn at. A crop's pixels are a third of the
/// view's, the pixels of a display three times as dense: its marks are drawn at that display's
/// scale, so the nine crops show them as wide as the whole view does.
fn rig_mark_dpi(dpi: f32) -> f32 {
    if rig_crop().is_some() { 3.0 * dpi } else { dpi }
}

/// rig: `vgrab`'s picture (the finished still view, the viewport alone at its own pixels, read
/// back as RGBA) written to `<mode file>.vgrab.png`, through a file beside it.
fn rig_write_vgrab(width: usize, height: usize, rgba: &[u8]) {
    use makepad_widgets_core::makepad_zune_png::{
        makepad_zune_core::{bit_depth::BitDepth, colorspace::ColorSpace, options::EncoderOptions},
        PngEncoder,
    };
    let Some(mode) = std::env::var_os("FAB_XR_FAR_MODE_FILE") else { return };
    let path = std::path::PathBuf::from(format!("{}.vgrab.png", mode.to_string_lossy()));
    let Some(rgba) = rgba.get(..width * height * 4) else {
        log!("fab vp vgrab: {} bytes for {width}x{height}", rgba.len());
        return;
    };
    let opaque: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let options = EncoderOptions::default().set_width(width).set_height(height).set_depth(BitDepth::Eight).set_colorspace(ColorSpace::RGBA);
    let mut out = Vec::new();
    if let Err(e) = PngEncoder::new(&opaque, options).encode(&mut out) {
        log!("fab vp vgrab: not encoded: {e:?}");
        return;
    }
    let part = path.with_extension("part");
    match std::fs::write(&part, &out).and_then(|_| std::fs::rename(&part, &path)) {
        Ok(()) => log!("fab vp vgrab: {width}x{height} to {}", path.display()),
        Err(e) => log!("fab vp vgrab: not written to {}: {e}", path.display()),
    }
}

/// Overlay strengths written into `DrawFabComposite.u_flags`: the selection
/// outline (0/1) and the cavity strength. Solid is clay with a subtle cavity
/// (~0.25, ridge/valley at full resolution); Realtime draws neither CAD
/// effect, its look is the renderer's.
fn shading_overlay_flags(vs: &ViewportState) -> [f32; 2] {
    let outlines = if vs.overlays.outlines { 1.0 } else { 0.0 };
    // Solid's clay shows its valleys. Realtime's creases are its screen AO's alone (no
    // double darkening); its convex edges catch a light (`realtime_edge_light`).
    let cavity = if vs.shading == Shading::Solid && vs.overlays.cavity { 0.25 } else { 0.0 };
    [outlines, cavity]
}

/// The composite's edge light in Realtime (`u_flags.w`): a convex edge, where the view normal
/// turns sharply away across a few pixels, lightens by up to this share, as a fillet catches
/// the studio's light; 0 in Solid and with the cavity overlay off.
fn realtime_edge_light(vs: &ViewportState) -> f32 {
    if vs.shading == Shading::Realtime && vs.overlays.cavity { REALTIME_EDGE_LIGHT } else { 0.0 }
}

/// The renderer's screen AO at the plate's scale (Fab's world is metres): a crease's and a
/// part's foot's within two centimetres. Carry the previous radius and strength into CORTAO.
const PLATE_AO_RADIUS: f32 = 0.02; // CORTAO: provisional, tuned from frames
const PLATE_AO_STRENGTH: f32 = 0.7; // CORTAO: provisional, tuned from frames
/// The board's light under the Sliced view: a ghost of it among the toolpaths, which it never joins.
const BOARD_GHOST: f32 = 0.35;

/// The Sliced view's smallest occluder (metres): its widest bead or tallest layer, whichever is
/// more (the old screen AO's 0.4 mm bias, from the print). The beads' ridges, the grooves
/// between them and the layer lines are shallower, so a flat printed face takes no AO from its
/// own relief, while a part's foot or an overhang (many layers) still does. Gaps in a live
/// preview's published layers do not describe the relief of a single layer.
fn sliced_min_occluder(preview: &crate::print::PrintPreview) -> f32 {
    let layers = &preview.layers;
    let first = layers.first().filter(|l| l.layer == 0).map_or(0.0, |l| l.z.max(0.0));
    let tallest = layers.windows(2)
        .filter(|w| w[0].layer.checked_add(1) == Some(w[1].layer))
        .map(|w| w[1].z - w[0].z)
        .fold(first, f32::max);
    let widest = preview.blocks.iter()
        .chain(preview.chunks.iter().flat_map(|chunk| chunk.runs.iter()))
        .map(|b| b.width)
        .fold(0.0, f32::max);
    // the preview is in millimetres
    tallest.max(widest).max(0.0) * 0.001
}

/// How much of the direct light the AO takes: the renderer's model lanes' `ao_direct`
/// (libs/render shaders, `mix(1, ao, mix(0.75, 0.35, linear))`), so a crease, a pore and a
/// part's foot read in the key's light too, not only in its shadow where a studio's fill is a
/// fiftieth of it. The screen AO's own hook in the renderer takes the ambient alone; the
/// composite adds this part.
fn ao_direct_weight(hdr: bool) -> f32 {
    if hdr { 0.35 } else { 0.75 }
}

/// `renderer`'s own sun rig for the field parts' shading (render frame, Y up), in its HDR lane
/// (`hdr`) or the legacy one: a field part lit as its meshes are.
pub fn tile_light(renderer: &Renderer, world: &makepad_scene::World, hdr: bool) -> TileLight {
    let sun = renderer.frame_sun(world);
    TileLight {
        dir: [sun.dir.x, sun.dir.y, sun.dir.z],
        color: [sun.color.x, sun.color.y, sun.color.z],
        sky: [sun.sky.x, sun.sky.y, sun.sky.z],
        ground: [sun.ground.x, sun.ground.y, sun.ground.z],
        up: [0.0, 1.0, 0.0],
        // the HDR lane's composite tone maps; the legacy lane's lit pass is display-encoded where
        // the meshes' is (the renderer's display transform), else written as lit
        display: !hdr && renderer.display_transform(),
        // The HDR lane's sun is radiometric: its lobe is lifted as the meshes' are.
        sun_spec: if hdr { std::f32::consts::PI } else { 1.0 },
        key_spec: renderer.key_spec(world, sun.dir),
        key_tint: renderer.ibl_key_tint(),
        // a floored paint's light, in the renderer's convention as the meshes' (lin_ctl.z)
        floor_gain: 1.0 / renderer.exposure(),
        ..TileLight::default()
    }
}

/// Whether the screen AO runs: in Realtime, as the renderer's lit scenes have it. Solid stays
/// clean clay with its own cavity.
fn wants_ssao(vs: &ViewportState) -> bool {
    vs.shading == Shading::Realtime
}

/// Combine the section state into at most six world half-spaces to KEEP,
/// in Fab space. `SectionState` documents the six-plane ceiling.
#[derive(Clone, Copy, PartialEq)]
struct SectionPlanes {
    values: [[f32; 4]; 6],
    len: usize,
}

impl SectionPlanes {
    fn as_slice(&self) -> &[[f32; 4]] { &self.values[..self.len] }
}

/// What the view cuts the plate to: the picked test print's box, or else the section.
fn view_planes(state: &AppState) -> SectionPlanes {
    if state.test_cut_shown() {
        if let Some(cube) = crate::ui::test_prints::shown(state) {
            return cube_planes(&cube);
        }
    }
    section_planes(&state.scene_state.section)
}

/// The picked test print's box as the lit meshes take it: the renderer's frame, metres. The
/// composite reads what stands behind a cut from the lit picture (the bed under a part cut away),
/// so what's cut from the G-buffer must be gone from there too.
fn lit_box(state: &AppState) -> Option<[[f32; 4]; 3]> {
    state.test_cut_shown().then(|| crate::ui::test_prints::shown(state)).flatten().map(|cube| lit_rows(&cube))
}

fn lit_rows(cube: &TestPrintView) -> [[f32; 4]; 3] {
    let axes = cube.axes();
    std::array::from_fn(|k| {
        let n = [axes[0][k], axes[1][k], axes[2][k]];
        let along = (n[0] * cube.center[0] + n[1] * cube.center[1] + n[2] * cube.center[2]) * 0.001;
        let half = cube.size[k] * 0.0005;
        let r = pack::to_render(vec3(n[0] as f32, n[1] as f32, n[2] as f32));
        [r.x / half as f32, r.y / half as f32, r.z / half as f32, (-along / half) as f32]
    })
}

/// A box's six sides as the half-spaces to keep, in Fab space (metres).
fn cube_planes(cube: &TestPrintView) -> SectionPlanes {
    let mut out = SectionPlanes { values: [[0.0; 4]; 6], len: 6 };
    let axes = cube.axes();
    for k in 0..3 {
        let n = [axes[0][k], axes[1][k], axes[2][k]];
        let along = (n[0] * cube.center[0] + n[1] * cube.center[1] + n[2] * cube.center[2]) * 0.001;
        let half = cube.size[k] * 0.0005;
        out.values[2 * k] = [n[0] as f32, n[1] as f32, n[2] as f32, (half - along) as f32];
        out.values[2 * k + 1] = [-n[0] as f32, -n[1] as f32, -n[2] as f32, (half + along) as f32];
    }
    out
}

fn section_planes(section: &SectionState) -> SectionPlanes {
    let mut out = SectionPlanes { values: [[0.0; 4]; 6], len: 0 };
    if !section.enabled {
        return out;
    }
    for p in &section.planes {
        if !p.enabled || out.len >= 6 {
            continue;
        }
        out.values[out.len] = [p.plane.a, p.plane.b, p.plane.c, p.plane.d];
        out.len += 1;
    }
    if let Some(b) = section.boxed {
        // Keep the inside: six inward-facing planes.
        for (n, d) in [
            ([1.0, 0.0, 0.0], -b.min.x),
            ([-1.0, 0.0, 0.0], b.max.x),
            ([0.0, 1.0, 0.0], -b.min.y),
            ([0.0, -1.0, 0.0], b.max.y),
            ([0.0, 0.0, 1.0], -b.min.z),
            ([0.0, 0.0, -1.0], b.max.z),
        ] {
            if out.len >= 6 {
                break;
            }
            out.values[out.len] = [n[0], n[1], n[2], d];
            out.len += 1;
        }
    }
    out
}

// ===========================================================================
// The widget
// ===========================================================================

fn render_scene_bounds(bounds: &Aabb) -> Option<(Vec3f, Vec3f)> {
    if aabb_is_empty(bounds) {
        return None;
    }
    // `to_render` negates Fab Y, so transform the opposite endpoints on the
    // renderer Z axis to keep this an ordered AABB.
    Some((
        vec3(bounds.min.x, bounds.min.z, -bounds.max.y),
        vec3(bounds.max.x, bounds.max.z, -bounds.min.y),
    ))
}

fn csm_far_range(bounds: &Aabb, camera: &Camera) -> f32 {
    let Some((min, max)) = render_scene_bounds(bounds) else {
        return DEFAULT_CSM_CONFIG.far_range;
    };
    let eye = pack::to_render(camera.eye);
    let mut far = 0.0f32;
    for bits in 0..8 {
        let corner = vec3(
            if bits & 1 == 0 { min.x } else { max.x },
            if bits & 2 == 0 { min.y } else { max.y },
            if bits & 4 == 0 { min.z } else { max.z },
        );
        far = far.max((corner - eye).length());
    }
    (far * 1.05).max(0.05)
}

#[derive(Script, WidgetRef, WidgetRegister)]
pub struct FabViewport {
    #[rust]
    scene: Option<Rc<RefCell<SceneGpu>>>,
    #[rust]
    parked: bool,
    #[rust]
    pointer_input: Option<ViewportInput>,
    #[rust]
    on_monitor_camera: bool,
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    /// Live diagnostics: `debug_view` is 0 normal, 1 lit, 2 aux, 3 screen AO.
    /// `debug_off` bits disable 8 the traced parts' cascade shadow, 32 screen AO, 64 renderer shadow maps,
    /// 128 cavity, 512 output dither, 1024 See-Through's half-resolution march (a hi-dpi
    /// display marches every pixel while the view moves too).
    /// Set live with `tweak apply path=<viewport path> prop=debug_view value=N`
    /// (or `prop=debug_off` for the bitmask).
    #[live(0.0)]
    debug_view: f64,
    #[live(0.0)]
    debug_off: f64,
    #[rust]
    debug_applied: Option<(f64, f64)>,
    #[live]
    draw_bg: DrawFabSceneTexture,
    #[live]
    draw_aux: DrawFabAux,
    #[live]
    draw_comp: DrawFabComposite,
    #[live]
    draw_rebuild: DrawFabRebuild,
    #[live]
    draw_glow_over: DrawFabGlowOver,
    #[live]
    draw_crosshair: DrawFabCrosshair,
    #[live]
    draw_control_ring: DrawFabControlRing,
    #[live]
    draw_grid: DrawFabGrid,
    #[live]
    draw_volume: DrawFabLines,
    #[live]
    draw_outline: DrawFabLines,
    /// The ring round the printing nozzle's tip (`print::HEAD_RING_MM`).
    #[live]
    draw_head_ring: DrawFabLines,
    /// The picks' outlines: what a click would take, the picks, the one whose chat chip is lit.
    #[live]
    draw_pick_under: DrawFabPickLines,
    #[live]
    draw_pick_lines: DrawFabPickLines,
    #[live]
    draw_pick_lit: DrawFabPickLines,
    #[live]
    draw_resin_layer: DrawFabResinLayer,
    #[live]
    draw_see: DrawFabSeeThrough,
    #[live]
    draw_see_up: DrawFabSeeUp,
    // libs/render's lanes, themed from script like every other draw struct.
    #[live]
    draw_sky: DrawSceneSky,
    #[live]
    draw_sky_analytic: DrawSceneSkyAnalytic,
    #[live]
    draw_shadow: DrawSceneShadow,
    #[live]
    draw_models: DrawSceneSkinned,
    // The print preview's beads and nozzle heads (`print.rs`), shaded in the aux pass: near
    // beads as tubes, far ones as quads (`far_tiers`).
    #[live]
    draw_path_aux: print::DrawFabPathAux,
    #[live]
    draw_far_aux: print::DrawFabFarAux,
    #[rust]
    far_tiers: print::FarTiers,
    #[live]
    draw_issue_spot: print::DrawFabIssueSpot,
    /// The Sliced view's hovered and picked branches' own beads, lit ([`BranchSpot`]): the
    /// slice's frame draws them over the beads they light.
    #[rust]
    branch_spot: Option<BranchSpot>,
    /// Where the picked branch stands on the screen as last drawn: the box of its discs (points;
    /// min x, min y, max x, max y), for the report.
    #[rust]
    picked_box: Option<[f64; 4]>,
    /// The frame actually submitted to the composite (also exposed to inspection).
    #[rust]
    issue_spot_drawn: Option<(crate::print::IssueSpot, f32)>,
    #[live]
    draw_head_aux: print::DrawFabHeadAux,
    #[rust]
    print_palette: print::PrintPalette,
    // The Force render mode's deformed mesh (`force.rs`), lit and aux, its surface and picks.
    #[live]
    draw_force: force::DrawFabForce,
    #[live]
    draw_force_aux: force::DrawFabForceAux,
    #[live]
    draw_board: board::DrawFabBoard,
    #[live]
    draw_board_aux: board::DrawFabBoardAux,
    #[live]
    draw_glass: glass::DrawFabGlass,
    /// The component bodies on the boards (`bodies.rs`), lit and aux.
    #[live]
    draw_body: bodies::DrawFabBody,
    #[live]
    draw_body_aux: bodies::DrawFabBodyAux,
    #[live]
    draw_body_glow: bodies::DrawFabBodyGlow,
    #[live]
    draw_body_screen: bodies::DrawFabBodyScreen,
    /// Allocated only while bound pipes emit.
    #[rust]
    draw_part_glow: Option<part_glow::DrawFabPartGlow>,
    #[live]
    draw_glow_pool: pool::DrawFabGlowPool,
    #[rust]
    pool_pane: bodies::PanePool,
    /// Draws the boards' layer art tiles from their shared drawings (`board.rs` `raster_art`).
    #[live]
    board_raster: fab_elec_view::board::art::ArtRaster,
    #[rust]
    board_moves: Vec<Option<Mat4f>>,
    /// Board moves for the traced scene when this pane shows a different pose source.
    #[rust]
    shared_board_moves: Vec<Option<Mat4f>>,
    #[rust]
    traced_board_poses: Vec<Mat4f>,
    /// What its pane is for: a Plate view has the tools, markers, branches and the force tip.
    #[rust]
    role: PaneRole,
    /// A click on a part's body selects it in the Electronics views, a click on none clears it
    /// ([`Self::select_part_at`]): the Device view's.
    #[live(false)]
    selects_parts: bool,
    #[rust]
    pane: PaneId,
    #[rust]
    cameras_frame: Option<u64>,
    /// The study's floating tip, while it is up: its text.
    #[rust]
    force_tip: Option<String>,
    /// The Force tool's pointer as the app last heard it: the point under it and Alt.
    #[rust]
    force_under: Option<Option<([f64; 3], bool)>>,
    /// Where a press of the Force tool went down.
    #[rust]
    force_press: Option<DVec2>,
    #[live]
    sdf_tile: fab_sdf_tiles::draw::DrawSdfTile,
    #[live]
    sdf_tile_lit: fab_sdf_tiles::draw::DrawSdfTileLit,
    /// What a field part's picked features, and the one under the pointer, mix toward.
    #[live]
    color_pick: Vec4f,
    /// Still-view supersampling: a view left still averages jittered recordings of its
    /// passes into an anti-aliased picture, then records nothing more.
    #[rust]
    still_aa: StillAa,
    /// The plate parts the recording on display drew: by the engine or as meshes.
    #[rust]
    parts_drawn: usize,
    /// The plate (`PlateInfo::revision`) and the scene showing it (`Scene::generation`) the last
    /// picture was taken of: other content (a new plate, or its scene installed) has its next
    /// settled frame read back for the plate list's pictures (saved with the plate). The view's
    /// own state (the camera, a layer step) takes none.
    #[rust]
    picture_of: Option<(u64, u64)>,
    #[rust]
    frame_ticket: Option<ReadbackTicket>,
    /// The settled composite shrunk to the picture's size, on the GPU (what is read back).
    #[rust]
    picture: picture::Picture,
    /// The plate's revision the pending read-back was asked at.
    #[rust(0u64)]
    frame_revision: u64,
    /// rig: `vgrab` in the reference rig's mode file reads back each finished still view whole
    /// (`rig_write_vgrab`): due from a sample added in that mode until its grab begins, and the
    /// grab. The view does not report itself settled meanwhile.
    #[rust]
    rig_vgrab_due: bool,
    #[rust]
    rig_grab: picture::ViewGrab,
    /// The project's picture or the rig's grab waits for its draw's shader: the next
    /// `Event::DrawShadersReady` draws again (a pipeline that turns ready while nothing draws it
    /// sends no repaint).
    #[rust]
    picture_waits_shader: bool,
    /// ... or for the next frame (the script VM was busy making its draw).
    #[rust]
    picture_waits_frame: bool,
    /// Monitor's estimated head is moving: this view is drawn again within a few frames, so a
    /// still average of it would never be seen.
    #[rust]
    head_moves: bool,
    /// The recording's noise seed: the still sample being recorded (0 otherwise), so the
    /// stochastic filters' patterns differ between the averaged samples.
    #[rust(0u32)]
    noise_seed: u32,
    /// The lit pass holds linear scene light (RGBA16F, the renderer's HDR lane) and the
    /// composite exposes and tone maps it; where the GPU cannot blend into float targets,
    /// the lanes tone map into an 8-bit lit pass themselves.
    #[rust(false)]
    hdr: bool,
    /// FAB_SDF_PROF: the scene passes are recorded every frame (a still view must record
    /// its passes to measure them) and their GPU times logged (`sdf_gpu_prof`).
    #[rust(std::env::var_os("FAB_SDF_PROF").is_some())]
    sdf_prof_enabled: bool,
    /// FAB_SDF_PROF: when the GPU profiler's recording was last asked for (Cx::time_now).
    #[rust(0.0)]
    sdf_prof_asked: f64,
    #[rust(std::env::var_os("FAB_PARAM_LOG").is_some())]
    param_log: bool,
    /// FAB_SDF_PROF: GPU frame totals, logged every 60 frames.
    #[rust]
    sdf_prof: Vec<f64>,
    #[rust(0u64)]
    sdf_prof_from: u64,
    /// FAB_SDF_PROF: the slowest frame of the window, its passes (name, ms) slowest first.
    #[rust]
    sdf_prof_slowest: Option<(f64, Vec<(String, f64)>)>,
    /// FAB_SDF_PROF: each pass's GPU time in the frames it ran in (name, ms), for its median
    /// over the window's frames.
    #[rust]
    sdf_prof_passes: Vec<(String, Vec<f64>)>,
    /// Brings the view back when a look the tracer owes falls due ([`sdf::SdfLayer::look_due_in`]).
    #[rust]
    sdf_timer: Timer,
    /// Brings the parts' scan back once the user's input has rested.
    #[rust]
    scan_timer: Timer,
    /// The lit pass's root list: the renderer's list nests in it, and the
    /// print preview's beads and the SDF parts follow it.
    #[new]
    print_list: DrawList,
    #[live(vec4(0.247, 0.247, 0.247, 1.0))]
    clear_color: Vec4f,
    #[new]
    aux_pass: DrawPass,
    /// The ghost layer's own G-buffer pass (`SdfPass::GhostAux`, the aux shader's ghost mode).
    #[new]
    ghost_pass: DrawPass,
    #[new]
    ghost_list: DrawList,
    #[new]
    ghost_texture: Texture,
    #[new]
    ghost_depth: Texture,
    /// The last recorded frame had parts drawn as ghosts.
    #[rust]
    ghost_on: bool,
    /// Original colour texel under an independently drawn copy.
    #[rust]
    copy_tint: Option<((ElementId, u32), f32)>,
    #[new]
    lit_pass: DrawPass,
    #[new]
    glow_pass: DrawPass,
    #[new]
    glow_list: DrawList,
    #[new]
    glow_texture: Texture,
    /// Aux's last recorded camera, including the still sample's jitter.
    #[rust]
    aux_state: Option<SceneState3D>,
    #[rust]
    aux_records: u64,
    #[rust]
    lit_records: u64,
    #[rust]
    glow_painted: Option<GlowKey>,
    #[new]
    comp_pass: DrawPass,
    #[new]
    aux_list: DrawList,
    #[new]
    lit_list: DrawList,
    #[new]
    comp_list: DrawList,
    #[new]
    aux_texture: Texture,
    #[new]
    lit_texture: Texture,
    #[new]
    comp_texture: Texture,
    /// See-Through's march on a hi-dpi display ([`SEE_HALF_DPI`]): its own pass at half the
    /// composite's pixels each way, which `draw_see_up` takes up into the composite.
    #[new]
    see_pass: DrawPass,
    #[new]
    see_list: DrawList,
    #[new]
    see_texture: Texture,
    /// The composite last recorded took See-Through from `see_pass`.
    #[rust]
    see_half: bool,
    #[new]
    depth_texture: Texture,
    #[rust]
    area: Area,
    #[rust(false)]
    initialized: bool,
    /// A device reset invalidates the retained desktop image until it paints again.
    #[rust]
    desktop_invalidated_at: u64,
    /// A pane without the renderer lease keeps its camera pass state in a fork.
    #[rust]
    review: Option<Box<Renderer>>,
    #[rust]
    review_generation: Option<u64>,
    #[rust]
    review_environment: Option<(Option<TextureId>, u64)>,

    #[rust]
    ground_geometry: Option<Geometry>,
    #[rust]
    volume_geometry: Option<(Volume, Geometry)>,
    /// The model's outline at the shown layer as lines: made from that layer (index) of each
    /// part's contours (their ids).
    #[rust]
    outline_geometry: Option<(u32, Vec<u64>, Geometry)>,
    /// That ring as lines, and the tip it was made round.
    #[rust]
    head_ring: Option<([f32; 3], Geometry)>,
    /// The picks' outlines as lines, by [`PickLines`] group: the vertices they were made of.
    #[rust]
    pick_geometry: [Option<(Vec<f32>, Geometry)>; 3],
    /// The quad a resin layer's image lies on: the stack's footprint on the bed (its pixels and
    /// their pitch), lifted to each layer's top as it draws.
    #[rust]
    resin_quad: Option<([u32; 4], [u64; 2], Geometry)>,
    #[rust]
    see_box: Option<Geometry>,
    /// The resin cut the retained scene passes clip at (`clip_cut`); a new layer's moves it there
    /// in place.
    #[rust]
    cut_drawn: Option<[f32; 4]>,
    /// The picked test print's box the last frame clipped to (centre and size, mm), for the
    /// inspection: which cube a finished picture shows.
    #[rust]
    test_cut: Option<([f64; 3], [f64; 3])>,
    #[rust]
    elem_lut: ElementLut,
    /// This viewport's CORTAO chain between `aux` and `lit`, using aux depth and normals.
    #[rust]
    ssao: CortaoPass,
    #[rust]
    ssao_table: Option<CortaoTable>,
    #[rust]
    ssao_texture: Option<Texture>,
    /// Retry a pending AO pipeline when DrawShadersReady arrives, before starting StillAa.
    #[rust]
    ssao_waiting: bool,
    /// The temporal history starts afresh at the next recording: a known view switch or camera
    /// fit, or any change to what the scene passes draw but the camera (`SceneKey::same_but_camera`,
    /// a cut moved in place).
    #[rust(true)]
    ssao_reset: bool,
    /// Runs since the history started: the AO's sample seed, so a picture after a change is the
    /// same as a fresh recording's.
    #[rust]
    ssao_age: u32,
    /// Projection switches also arrive through the navigator's direct keyboard controls.
    #[rust]
    ssao_ortho: Option<bool>,
    /// A relief floor change invalidates the history from the last successful AO run.
    #[rust]
    ssao_floor: Option<f32>,
    /// Orbiting reuses a preview's relief floor without scanning its runs or keeping it alive.
    #[rust]
    ssao_floor_for: Option<(std::sync::Weak<crate::print::PrintPreview>, f32)>,
    /// The screen AO ran in the last scene recording (the composite reads it).
    #[rust]
    ssao_on: bool,
    /// This viewport's glow-layer halo, independent of the retained picture.
    #[rust]
    bloom: BloomPass,
    /// Display white in the glow layer's units at its last paint (its `glow_gain`): the halo's
    /// threshold, in the bloom and in the window quad alike.
    #[rust(1.0)]
    glow_white: f32,
    #[rust]
    navigator: Navigator,
    /// Only a Traced pane creates a GPU tracer; Raster holds no trace resources.
    #[rust]
    traced: Option<product_scene::TracedPane>,
    #[rust]
    pointer_held: bool,
    #[rust]
    tools: ToolSet,
    #[rust]
    last_pointer: Option<DVec2>,
    #[rust]
    context_click: bool,
    /// A press the marking menu took. Esc may close the ring before its button comes up.
    #[rust]
    menu_press: bool,
    #[rust]
    hovered_marker: Option<usize>,
    #[rust]
    pressed_marker: Option<usize>,
    /// The branch of the shown slice's supports under the pointer, and the one pressed: its
    /// part's object and its tips, found again in each new list of trees
    /// ([`crate::print::PrintState::branch_of`]).
    #[rust]
    hovered_branch: Option<(String, Vec<[f64; 3]>)>,
    #[rust]
    pressed_branch: Option<(String, Vec<[f64; 3]>)>,
    /// The no-trees area pressed (`instance/solid`) ([`FabViewport::pick_area`]).
    #[rust]
    pressed_area: Option<String>,
    /// A primary press on an issue marker, a branch or a no-trees area: the navigator's alone, no
    /// tool's, so a drag from it orbits as one from anywhere else does, and a click (a release
    /// without a drag, still on what was pressed) picks what it was on. A drag lets that go.
    #[rust]
    press_claimed: bool,
    /// A simulation press owns input before the navigator; its drawn state comes from SimLive.
    #[rust]
    live_press: press::LivePress,
    /// The panel that owns the pointer until release, even when the finger slides off its glass.
    #[rust]
    live_touch: Option<fab_elec_core::CompRef>,
    #[rust]
    touch_texel: (f32, f32),
    /// A knob turned by a drag (a printed knob bound to a pot, `AppState::part_turns`, or a board
    /// pot's shaft): the navigator's input until its release, as a live press is.
    #[rust]
    live_turn: press::LiveTurn,
    /// The turned knob's readout while the drag lasts: where it stands (window points, by the
    /// knob the drag took) and the knob's name.
    #[rust]
    turn_tip: Option<(DVec2, String)>,
    /// The control the pointer rests on, its name shown by it ([`Self::show_control_tip`]).
    #[rust]
    hover_control: Option<fab_elec_core::CompRef>,
    /// The live controls' grips as the last draw showed them ([`Self::collect_grips`]).
    #[rust]
    control_grips: Vec<ControlGrip>,
    #[rust]
    press_digit: Option<DigitId>,
    #[rust]
    next_frame: NextFrame,
    /// The rebuilding effect on a part: which part, when it started, when it settled.
    #[rust]
    rebuild_fx: Option<RebuildFx>,
    #[rust]
    last_frame_time: f64,
    #[rust(0u32)]
    draw_calls: u32,
    #[rust(0u32)]
    debug_frames: u32,
    /// What the scene passes were last recorded from (`SceneKey`); the composite's extra input.
    #[rust]
    scene_key: Option<SceneKey>,
    /// Scratch for the next key and the print snapshot; their vectors keep capacity.
    #[rust]
    spare_scene_key: Option<SceneKey>,
    /// Live part rows last observed by the glow layer. Publications never change scene content,
    /// the retained lit passes or the still average.
    #[rust]
    body_live_painted: BodyLiveKeys,
    /// Each pane tracks held publications independently, including when a traced pane syncs
    /// the shared board layer before a raster pane records it.
    #[rust]
    body_held_drawn: Option<Arc<Vec<fab_elec_core::CompRef>>>,
    #[rust]
    screen_layout_drawn: screen::ScreenLayout,
    /// The last lit recording drew an emitting lens or screen glass.
    #[rust]
    body_emitting: bool,
    #[rust]
    pipe_glow_key: Option<u64>,
    #[rust]
    print_frame: Option<print::PrintFrame>,
    /// What the composite was last recorded from: the hovered part and the shown resin layer.
    #[rust]
    comp_key: Option<(Option<ElementId>, Option<u64>)>,
    /// Record the scene passes on the next draw whatever the key says (first draw, GPU reset,
    /// a script reapply, a renderer still settling).
    #[rust(true)]
    force_record: bool,
    /// Since when the recorded scene waits for the renderer (`items_ready`: a pipeline, the
    /// environment): it records again when one of those answers (`Event::DrawShadersReady`, the
    /// environment's job ending), never every frame. The still view waits too.
    #[rust]
    items_wait: Option<f64>,
    /// Says once what a wait past `ITEMS_WAIT_SAY` seconds waits for.
    #[rust]
    items_wait_timer: Timer,
    /// Its camera follows another pane or has followers ([`Self::camera_moved_elsewhere`]).
    #[rust]
    shares_camera: bool,
    /// Draws so far (the description's `records.draws`).
    #[rust]
    draws: u64,
    /// How often a publication asked the view to draw (the description's `records.published`): the
    /// board's live values (its lights, screens and rows), a board drawing or a held press coming in,
    /// a screen's layout. A test tells a publication's draws from its own reads'.
    #[rust]
    published_redraws: u64,
    /// Where the last pointer move over the view was, in window points (the description's
    /// `pointer`): a test knows the view has taken every move it sent.
    #[rust]
    pointer_seen: Option<DVec2>,
    /// `FAB_VP_STATS=1`: once a second while drawing, how many draws recorded what.
    #[rust(std::env::var_os("FAB_VP_STATS").is_some())]
    stats_log: bool,
    /// Draws, scene recordings, composite recordings since `stats_since`.
    #[rust]
    stats: [u32; 3],
    /// The scene recordings for a change (a still view's samples aside) and the resin cut's
    /// moves in place, since the start (the inspection's `records`).
    #[rust]
    scene_records: u64,
    #[rust]
    cuts_moved: u64,
    /// How often the still view's average started again (the inspection's `records.still`), and how
    /// often for a change of the composite alone, the scene unchanged (`records.comp`).
    #[rust]
    still_restarts: u64,
    #[rust]
    comp_restarts: u64,
    /// The element the pointer over this view is on (the inspection's `hover`).
    #[rust]
    hover_seen: Option<ElementId>,
    /// The scene passes hold a still view's jittered sample (the composite does not).
    #[rust]
    scene_jittered: bool,
    /// `AppState::elec_previewing` at the last draw: an edit's preview moves what the view
    /// shows, so the view is not still (`still_settled`) while it lasts.
    #[rust]
    edit_moving: bool,
    /// A scene this view will show is on its way (`AppState::scene_coming`): the view does not
    /// report itself settled until it has come.
    #[rust]
    scene_coming: bool,
    #[rust]
    stats_since: f64,
}

/// Everything the aux and lit passes (and so the composite) are recorded from. Equal keys:
/// the retained pictures are still right and nothing is recorded.
/// A second instance in the existing ghost pass. Its geometry is already in plate metres;
/// its source element identifies it to the compositor without adding a pickable scene element.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct GhostCopy {
    pub geometry: GeometryId,
    pub source: ElementId,
    /// Packed sRGB, the representation the ghost compositor reads.
    pub tint: u32,
}

#[derive(Clone, PartialEq)]
struct SceneKey {
    ghost_copy: Option<GhostCopy>,
    size: DVec2,
    dpi: f64,
    generation: u64,
    revision: u64,
    /// The pane's pose set and its revision, and the shared layers'.
    poses: (Option<(PoseSetId, u64)>, Option<(PoseSetId, u64)>),
    camera: Camera,
    shading: Shading,
    overlays: Overlays,
    xray: bool,
    clip: SectionPlanes,
    /// A resin layer cuts the parts (where: `clip_cut`, moved in place).
    cut: bool,
    caps: bool,
    sun: SkyState,
    print: Option<PrintKey>,
    force: Option<force::ForceFrame>,
    markers: Option<u64>,
    hovered_marker: Option<usize>,
    clear_color: Vec4f,
    /// The SDF parts: their revision (`sdf_revision`) and how parts with a field draw.
    sdf: u64,
    look: PartLook,
    lighting: Lighting,
    volume: Option<Volume>,
    /// See-Through: its opacity, its glow and what its grids hold (`SeeThroughLayer::revision`).
    see: Option<(u32, u32, u64)>,
    glass: bool,
    /// The parts the Electronics views selected (`AppState::elec_selected`): their bodies' ring.
    elec_selected: std::sync::Arc<[fab_elec_core::CompRef]>,
    /// The field parts' lit-up features (`sdf::tint_key`), drawn by the lit pass.
    tint: u64,
}

/// What a print preview frame draws (`print::PrintFrame`).
#[derive(Clone, PartialEq)]
struct PrintKey {
    preview: usize,
    cut: crate::print::RunCut,
    travel_count: usize,
    uniforms: [[f32; 4]; 4],
    heads: Vec<print::Head>,
    show_markers: bool,
    compare_planar: bool,
    show_supports: bool,
    faded: print::Faded,
    contours: Option<u64>,
    lit: Option<u64>,
    /// View › Sliced Detail: the far beads as quads or every bead whole.
    sliced_detail: SlicedDetail,
}

/// The viewport watches row identity, LED levels and screen frames independently of the retained scene.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct BodyLiveKeys {
    rows: Option<(Option<TextureId>, u64)>,
    lights: Option<u64>,
    screens: Option<u64>,
    playing: bool,
    lit: bool,
    screens_on: bool,
}

impl BodyLiveKeys {
    fn of(cx: &mut Cx) -> Self {
        Self::of_sim(if cx.has_global::<ElecLive>() { Some(&cx.get_global::<ElecLive>().sim) } else { None })
    }

    fn of_sim(sim: Option<&SimLive>) -> Self {
        Self {
            rows: sim.map(|sim| (sim.parts.as_ref().map(Texture::texture_id), sim.index.generation)),
            lights: sim.map(|sim| sim.lights),
            screens: sim.map(|sim| sim.screens_key),
            playing: sim.is_some_and(|sim| sim.playing),
            lit: sim.is_some_and(|sim| sim.emission > 0.0),
            screens_on: sim.is_some_and(|sim| sim.screens.iter().any(|s| s.on && s.brightness > 0.0)),
        }
    }
}

/// A glow layer follows live light and the exact scene samples it overlays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GlowKey {
    rows: Option<(Option<TextureId>, u64)>,
    lights: Option<u64>,
    screens: Option<u64>,
    aux: u64,
    lit: u64,
    pipes: Option<u64>,
}

impl GlowKey {
    fn of(emitting: bool, live: BodyLiveKeys, aux: u64, lit: u64) -> Option<Self> {
        (emitting && (live.lit || live.screens_on) && live.rows.is_some()).then_some(Self {
            rows: live.rows, lights: live.lights, screens: live.screens, aux, lit, pipes: None,
        })
    }
}

/// The retained picture's work. Live part rows are not inputs: LED levels reach only the
/// glow layer, so a playing bench leaves the still view free to finish its samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FramePlan {
    record_scene: bool,
    record_comp: bool,
    restart_still: bool,
    wants_sample: bool,
}

/// The part the composites tint under the pointer: the hovered one, unless the pane the pointer
/// is over tints none (`SceneGpu::hover_tints`).
fn hover_tint(state: &AppState, scene: &SceneGpu) -> Option<ElementId> {
    state.view.hover.filter(|_| scene.hover_tints).map(|hit| hit.element)
}

impl FramePlan {
    fn of(scene_changed: bool, comp_changed: bool, settled: bool, samples_left: bool) -> Self {
        let record_comp = scene_changed || comp_changed;
        Self {
            record_scene: scene_changed,
            record_comp,
            restart_still: record_comp,
            wants_sample: !record_comp && settled && samples_left,
        }
    }
}

impl SceneKey {
    /// The same scene drawn, the camera aside.
    fn same_but_camera(&self, o: &SceneKey) -> bool {
        let mut key = self.clone();
        key.camera = o.camera.clone();
        key == *o
    }

    /// The first field that differs from `other` (the stats log's reason for a recording).
    fn first_difference(&self, o: &SceneKey) -> &'static str {
        if self.size != o.size || self.dpi != o.dpi { "size" }
        else if self.generation != o.generation { "generation" }
        else if self.revision != o.revision { "revision" }
        else if self.poses != o.poses { "poses" }
        else if self.ghost_copy != o.ghost_copy { "ghost copy" }
        else if self.camera != o.camera { "camera" }
        else if self.shading != o.shading || self.overlays != o.overlays || self.xray != o.xray || self.look != o.look || self.lighting != o.lighting || self.volume != o.volume || self.see != o.see || self.glass != o.glass { "view" }
        else if self.clip != o.clip || self.cut != o.cut || self.caps != o.caps { "section" }
        else if self.sun != o.sun { "sun" }
        else if self.print != o.print || self.markers != o.markers || self.hovered_marker != o.hovered_marker { "print" }
        else if self.force != o.force { "force" }
        else if self.clear_color != o.clear_color { "clear" }
        else if self.sdf != o.sdf { "sdf" }
        else if self.elec_selected != o.elec_selected { "selection" }
        else { "none" }
    }

    #[allow(clippy::too_many_arguments)]
    fn of(state: &AppState, pane: PaneId, size: DVec2, dpi: f64, hovered_marker: Option<usize>, clear_color: Vec4f, sdf: u64, see: u64, planes: SectionPlanes, cut: bool, print_frame: Option<&print::PrintFrame>, force: Option<&force::ForceFrame>, previous: Option<SceneKey>) -> SceneKey {
        let mut heads = previous.and_then(|k| k.print).map_or_else(Vec::new, |p| p.heads);
        let print = print_frame.map(|f| {
            heads.clone_from(&f.heads);
            PrintKey {
                preview: std::sync::Arc::as_ptr(&f.preview) as usize,
                cut: f.cut.clone(),
                travel_count: f.travel_count,
                uniforms: f.uniforms.map(|(_, v)| v),
                heads,
                show_markers: f.show_markers,
                compare_planar: f.compare_planar,
                show_supports: f.show_supports,
                faded: f.faded.clone(),
                contours: state.print.contours().map(|l| l.contours.all.id()),
                lit: f.lit.as_ref().map(RetainedInstances::id),
                sliced_detail: state.view.sliced_detail,
            }
        });
        let vs = &state.view;
        SceneKey {
            ghost_copy: crate::tools::fit::ghost(state, pane),
            size,
            dpi,
            generation: state.scene.generation,
            revision: state.scene_state.revision,
            poses: (state.poses_key(state.pane_poses(pane)), state.poses_key(state.shared_poses())),
            camera: *vs.camera(pane),
            shading: vs.shading,
            overlays: vs.overlays,
            xray: vs.xray,
            clip: planes,
            cut,
            caps: state.scene_state.section.caps,
            sun: state.sun,
            markers: state.print.markers().map(|m| m.id()),
            print,
            force: force.cloned(),
            hovered_marker,
            clear_color,
            sdf,
            look: vs.look,
            lighting: vs.lighting_for(&state.ui),
            volume: Volume::of(state),
            see: vs.see.on.then(|| (vs.see.opacity.to_bits(), vs.see.glow.to_bits(), see)),
            glass: vs.glass,
            elec_selected: state.elec_selected.clone(),
            tint: sdf::tint_key(state),
        }
    }
}

/// A script reapply keeps the pane its stage or editor bound ([`FabViewport::bind_pane`]).
impl ScriptHook for FabViewport {}

impl FabViewport {
    /// The texture that shows the 3D view now and the view's size in points: what a picture of the
    /// view reads back (the marker's marks are drawn over it, not in it).
    pub fn view_texture(&self, cx: &Cx) -> (Texture, DVec2) {
        let texture = self.traced.as_ref().and_then(|trace| trace.texture()).cloned()
            .or_else(|| self.still_aa.output().cloned()).unwrap_or_else(|| self.comp_texture.clone());
        (texture, self.area.rect(cx).size)
    }

    /// The pointer input accepted by this viewport's area, for its visual tool overlays.
    pub(crate) fn take_pointer_input(&mut self) -> Option<ViewportInput> {
        self.pointer_input.take().filter(|_| self.role == PaneRole::Plate)
    }

    /// Bind an editor's viewport before dispatching events or drawing it. The editor retains the lease.
    pub fn set_pane(&mut self, lease: &PaneLease) {
        self.bind_pane(lease.id(), lease.role());
    }

    /// The pane it draws.
    pub fn pane(&self) -> PaneId {
        self.pane
    }

    /// Bind it to a pane it does not lease (a Plate stage's own).
    pub fn bind_pane(&mut self, pane: PaneId, role: PaneRole) {
        self.pane = pane;
        self.role = role;
        self.navigator.pane = pane;
        self.tools.pane = pane;
    }

    fn drop_fork(&mut self) {
        self.review = None;
        self.review_generation = None;
        self.review_environment = None;
    }

    fn adopt_scene(&mut self, state: &AppState) {
        if self.scene.as_ref().is_none_or(|s| !Rc::ptr_eq(s, &state.scene_gpu)) {
            self.scene = Some(state.scene_gpu.clone());
            self.drop_fork();
            self.force_record = true;
            self.ssao_reset = true;
        }
    }

    fn scene(&self) -> &Rc<RefCell<SceneGpu>> {
        self.scene.as_ref().expect("viewport adopted its app scene")
    }

    /// Withdraw this camera's GPU work. Drawing it again resumes with the same slot and camera.
    /// Its camera came back from the plate's view file: the next frame checks it
    /// ([`crate::nav::Navigator::check_camera`]).
    pub(crate) fn check_camera(&mut self) {
        self.navigator.check_camera();
    }

    pub fn park(&mut self, cx: &mut Cx, state: &mut AppState) {
        self.dismiss_menu(cx, state);
        self.park_widget(cx);
        state.park_pane(self.pane);
    }

    // A hidden stage no longer dispatches to the handles that normally close its ring.
    fn dismiss_menu(&self, cx: &mut Cx, state: &mut AppState) {
        if self.role == PaneRole::Plate && state.picks.menu.as_ref().is_some_and(|m| m.pane == self.pane) {
            state.picks.menu = None;
            cx.sweep_unlock(self.area);
            self.area.redraw(cx);
            crate::ui::redraw(cx, &[ids!(tool_overlay)]);
        }
    }

    /// What a viewport whose Plate the dock holds off screen still hears, each event handled as
    /// on screen: what resets what it holds (a GPU reset, shaders, a lost focus), its timers (one
    /// it does not hear stays armed for good), a wake-up only it can take (a parked view's
    /// readbacks, the project's picture, the host's studio and star map), and the camera
    /// commands while its pane is the active Plate. A camera jump still restarts its screen AO.
    /// The lead's scan takes its wait. What it skips only redraws, and a Plate shown again
    /// redraws ([`crate::ui::stage::FabStage::shown_again`]). O(1) per event it skips.
    pub fn handle_hidden(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get_mut::<AppState>() else { return };
        self.dismiss_menu(cx, state);
        let full = match event {
            Event::GpuReset | Event::ScriptReapply | Event::LiveEdit | Event::DrawShadersReady
            | Event::WindowLostFocus(_) | Event::Pause | Event::Background | Event::Timer(_) => true,
            Event::Actions(actions) => {
                let active = self.pane == state.view.active_plate();
                let mut full = false;
                for action in shell_actions(actions) {
                    if camera_jump(action) {
                        self.ssao_reset = true;
                    }
                    full |= active && camera_command(action);
                }
                full
            }
            Event::Signal => self.parked || self.frame_ticket.is_some() || state.scene_gpu.borrow().host == Some(self.pane),
            _ => false,
        };
        if full {
            self.handle_event(cx, event, scope);
        } else if matches!(event, Event::Signal) && self.pane == state.view.lead_plate() && state.view.pane(self.pane).mode == PaneMode::Raster {
            self.handle_scan(cx, event);
        }
    }

    /// Release this viewport's passes and input before its editor parks the pane lease.
    pub fn park_widget(&mut self, cx: &mut Cx) {
        if self.parked { return; }
        self.parked = true;
        self.release_press(cx);
        self.live_turn.up();
        self.show_turn_tip(cx, None);
        self.show_force_tip(cx, None);
        self.release_passes(cx);
        self.set_pane_mode(cx, PaneMode::Raster);
        self.pointer_held = false;
        self.press_claimed = false;
        self.press_digit = None;
        self.drop_fork();
        // a parked owner moves no camera: its linked panes take their samples
        self.publish_still(cx);
        self.navigator = Navigator::default();
        self.navigator.pane = self.pane;
        self.pointer_input = None;
        self.last_pointer = None;
        self.context_click = false;
        self.menu_press = false;
        self.hovered_marker = None;
        self.hovered_branch = None;
        self.pressed_marker = None;
        self.pressed_branch = None;
        self.pressed_area = None;
        self.force_press = None;
        self.next_frame = NextFrame::default();
        cx.stop_timer(self.items_wait_timer);
        cx.stop_timer(self.sdf_timer);
        cx.stop_timer(self.scan_timer);
        self.items_wait_timer = Timer::empty();
        self.sdf_timer = Timer::empty();
        self.scan_timer = Timer::empty();
        self.items_wait = None;
        self.scene_key = None;
        self.comp_key = None;
        self.body_live_painted = BodyLiveKeys::default();
        self.glow_painted = None;
        self.body_emitting = false;
        self.pipe_glow_key = None;
        self.draw_part_glow = None;
        self.elem_lut = ElementLut::default();
        self.still_aa = StillAa::default();
        self.ssao = CortaoPass::default();
        self.ssao_texture = None;
        self.ssao_on = false;
        self.ssao_table = None;
        self.ssao_waiting = false;
        self.ssao_reset = true;
        self.bloom = BloomPass::default();
        if let Some(ticket) = self.frame_ticket.take() {
            cx.cancel_texture_readback(ticket);
            self.picture_of = None;
        }
        self.picture = picture::Picture::default();
        self.picture_waits_frame = false;
        self.picture_waits_shader = false;
        self.rig_vgrab_due = false;
        self.rig_grab.drop_grab();
        self.cameras_frame = None;
        self.initialized = false;
        self.force_record = true;
    }

    fn release_passes(&mut self, cx: &mut Cx) {
        let mut targets: Vec<&Texture> = Vec::new();
        if self.initialized {
            for pass in [&self.aux_pass, &self.ghost_pass, &self.lit_pass, &self.glow_pass, &self.comp_pass, &self.see_pass] {
                pass.withdraw(cx);
                pass.clear_color_textures(cx);
                pass.clear_depth_texture(cx);
            }
            targets.extend([&self.aux_texture, &self.ghost_texture, &self.ghost_depth, &self.lit_texture,
                &self.glow_texture, &self.comp_texture, &self.depth_texture, &self.see_texture]);
            // Retire the retained draw calls, including their fork's cascade and field bindings.
            for list in [&mut self.aux_list, &mut self.ghost_list, &mut self.lit_list, &mut self.print_list,
                &mut self.glow_list, &mut self.comp_list, &mut self.see_list] {
                *list = DrawList::new(cx);
            }
        }
        targets.extend(self.still_aa.output());
        targets.extend(self.bloom.bloom());
        targets.extend(self.ssao_texture.as_ref());
        targets.extend(self.elem_lut.texture());
        targets.extend(self.traced.as_ref().and_then(product_scene::TracedPane::texture));
        if !targets.is_empty() {
            // The window quad can retain a target even after its own pass was withdrawn.
            let ids: Vec<_> = targets.iter().map(|texture| texture.texture_id()).collect();
            cx.draw_lists.release_bindings(&ids, &[]);
            for texture in targets { texture.release(cx); }
        }
        for vars in [&mut self.draw_bg.draw_vars, &mut self.draw_glow_over.draw_vars, &mut self.draw_comp.draw_vars,
            &mut self.draw_board.draw_vars, &mut self.draw_body.draw_vars, &mut self.draw_shadow.draw_vars,
            &mut self.draw_models.draw_vars, &mut self.sdf_tile.draw_vars, &mut self.sdf_tile_lit.draw_vars] {
            vars.texture_slots.fill(None);
        }
    }

    pub(crate) fn set_pane_mode(&mut self, cx: &mut Cx, mode: PaneMode) {
        if mode == PaneMode::Raster {
            if let Some(mut trace) = self.traced.take() {
                trace.close(cx);
                self.scene_key = None;
                self.comp_key = None;
                self.force_record = true;
                self.area.redraw(cx);
            }
            self.draw_bg.scene_ready = 1.0;
        }
    }

    pub(crate) fn trace_interacting(&self) -> bool {
        self.pointer_held || self.navigator.is_animating()
    }


    /// The lease holder draws the host. Other cameras keep a fork until a resident model is missing,
    /// as in Sandbox's split view. Borrow fields separately so layers can use it while drawing.
    fn pane_renderer<'a>(is_host: bool, host: &'a mut Renderer, review: &'a mut Option<Box<Renderer>>, model: &str) -> &'a mut Renderer {
        if is_host {
            return host;
        }
        if review.as_ref().is_none_or(|fork| !fork.model_is_loaded(model) && host.model_is_loaded(model)) {
            *review = Some(Box::new(host.fork_scene_for_review()));
        }
        review.as_deref_mut().unwrap()
    }

    fn ensure_initialized(&mut self, cx: &mut Cx) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        self.aux_texture = Texture::new_with_format(
            cx,
            TextureFormat::RenderRGBAf32 {
                size: TextureSize::Auto,
                initial: true,
            },
        );
        self.ghost_texture = Texture::new_with_format(
            cx,
            TextureFormat::RenderRGBAf32 {
                size: TextureSize::Auto,
                initial: true,
            },
        );
        self.ghost_depth = Texture::new_with_format(
            cx,
            TextureFormat::DepthD32 {
                size: TextureSize::Auto,
                initial: true,
            },
        );
        self.hdr = cx.gpu_info().float16_blend_targets;
        self.lit_texture = Texture::new_with_format(
            cx,
            if self.hdr {
                TextureFormat::RenderRGBAf16 { size: TextureSize::Auto, initial: true }
            } else {
                TextureFormat::RenderBGRAu8 { size: TextureSize::Auto, initial: true }
            },
        );
        self.glow_texture = Texture::new_with_format(
            cx,
            TextureFormat::RenderRGBAf16 { size: TextureSize::Auto, initial: true },
        );
        self.comp_texture = Texture::new_with_format(
            cx,
            TextureFormat::RenderBGRAu8 {
                size: TextureSize::Auto,
                initial: true,
            },
        );
        self.depth_texture = Texture::new_with_format(
            cx,
            TextureFormat::DepthD32 {
                size: TextureSize::Auto,
                initial: true,
            },
        );
        self.see_texture = Texture::new_with_format(
            cx,
            TextureFormat::RenderBGRAu8 {
                size: TextureSize::Auto,
                initial: true,
            },
        );
        for (p, name) in [(&self.aux_pass, "fab_aux"), (&self.ghost_pass, "fab_ghost"), (&self.lit_pass, "fab_lit"), (&self.glow_pass, "fab_glow"), (&self.comp_pass, "fab_comp"), (&self.see_pass, "fab_see")] {
            p.set_keep_camera_matrix(cx, true);
            // The profiler's names: aux, ghosts, lit, glow, composite and the half See-Through.
            p.set_pass_name(cx, name);
        }
    }

    /// The parts' scan runs only with the user at rest: any input stops it at once, and a timer
    /// brings it back. What it finds wakes the view as the tracer's tiles do ([`Self::handle_tracer`]).
    fn handle_scan(&mut self, cx: &mut Cx, event: &Event) {
        let scene = self.scene().clone();
        let scene = scene.borrow();
        let scan = scene.sdf.borrow().scan.clone();
        if matches!(event, Event::MouseDown(_) | Event::MouseMove(_) | Event::MouseUp(_) | Event::Scroll(_) | Event::TouchUpdate(_) | Event::KeyDown(_) | Event::KeyUp(_) | Event::TextInput(_)) {
            scan.input();
            if self.scan_timer.is_empty() {
                self.scan_timer = cx.start_timeout(fab_sdf_tiles::trace::scan::REST.as_secs_f64());
            }
        }
        if self.scan_timer.is_event(event).is_some() {
            self.scan_timer = Timer::empty();
            match scan.rest_left() {
                Some(left) => self.scan_timer = cx.start_timeout(left.as_secs_f64()),
                None => scan.kick(&cx.task_pool()),
            }
        }
        if matches!(event, Event::Signal) {
            if scan.take_waiting() && self.scan_timer.is_empty() {
                self.scan_timer = cx.start_timeout(scan.rest_left().map_or(0.0, |left| left.as_secs_f64()));
            }
        }
    }

    /// The field parts' tracer works on its own and wakes the view when it has tiles for it or
    /// its work cannot go on without a look; a look it owes later (a camera at rest, a shape left
    /// alone) comes back on a timer. A paint bake that ends wakes it the same way. The view
    /// records the scene for them then and only then: a still view between the tracer's tiles
    /// records nothing.
    ///
    /// A desktop a headset's interaction holds ([`makepad_xr_stream::XrDesktopFreeze`]) paints
    /// nothing: the look waits for the release, which redraws the whole window (the eyes trace
    /// and draw their own tiles every headset frame).
    fn handle_tracer(&mut self, cx: &mut Cx, event: &Event) {
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        if matches!(event, Event::Signal) && scene.sdf.borrow_mut().take_woke() {
            scene.sdf_look = true;
            if !makepad_xr_stream::XrDesktopFreeze::active(cx) {
                self.redraw(cx);
            }
        }
        if self.sdf_timer.is_event(event).is_some() {
            self.sdf_timer = Timer::empty();
            let due = scene.sdf.borrow().look_due_in();
            match due {
                Some(left) if !left.is_zero() => self.sdf_timer = cx.start_timeout(left.as_secs_f64()),
                Some(_) => {
                    scene.sdf_look = true;
                    if !makepad_xr_stream::XrDesktopFreeze::active(cx) {
                        self.redraw(cx);
                    }
                }
                None => {}
            }
        }
    }

    /// After a record: the view comes back when the next look the tracer owes falls due.
    fn time_tracer_look(&mut self, cx: &mut Cx) {
        let scene = self.scene().clone();
        let scene = scene.borrow();
        cx.stop_timer(self.sdf_timer);
        let due = scene.sdf.borrow().look_due_in();
        self.sdf_timer = match due {
            Some(left) => cx.start_timeout(left.as_secs_f64()),
            None => Timer::empty(),
        };
    }

    /// Camera → the two `SceneState3D`s. Both describe the same view of the
    /// same plate; they differ only by the fixed Z-up → Y-up turn, so the
    /// composed view-projection over a Fab point is identical in both.
    fn scene_states(&self, camera: &Camera, rect: Rect, time: f64) -> (SceneState3D, SceneState3D) {
        let aspect = (rect.size.x / rect.size.y.max(1.0)) as f32;
        let mut projection = camera.projection(aspect);
        // rig: `crop<x><y>` draws a third of the view over the whole viewport (the 9x reference)
        if let Some(crop) = rig_crop() {
            projection = rig_crop_projection(projection, crop);
        }
        let fab = SceneState3D {
            time,
            camera_pos: camera.eye,
            view: camera.view(),
            projection,
            viewport_rect: rect,
        };
        let render = SceneState3D {
            time,
            camera_pos: pack::to_render(camera.eye),
            view: Mat4f::look_at(
                pack::to_render(camera.eye),
                pack::to_render(camera.target),
                pack::to_render(camera.up),
            ),
            projection,
            viewport_rect: rect,
        };
        (fab, render)
    }

    // The ghost compositor reads colour from the source element's fourth texel. An opaque
    // source has no ghost colour of its own. Supply the copy's tint without changing the
    // source's visibility or pose in any pass. Only that texel is uploaded.
    fn sync_copy_tint(&mut self, cx: &mut Cx, copy: Option<GhostCopy>, table_changed: bool) {
        if table_changed { self.copy_tint = None; }
        let tint = copy.map(|g| (g.source, g.tint));
        if self.copy_tint.map(|(key, _)| key) == tint { return; }
        let Some(table) = self.elem_lut.texture() else { return };
        let width = self.elem_lut.size().0 as usize;
        let replace = |cx: &mut Cx, id: ElementId, colour: f32| {
            let texel = id.index() * STRIDE + 3;
            let mut data = table.take_vec_f32(cx);
            let old = std::mem::replace(&mut data[texel * 4 + 3], colour);
            table.put_back_vec_f32(cx, data, Some(RectUsize::new(PointUsize::new(texel % width, texel / width), SizeUsize::new(1, 1))));
            old
        };
        if let Some(((id, _), old)) = self.copy_tint.take() { replace(cx, id, old); }
        if let Some((id, rgb)) = tint {
            self.copy_tint = Some(((id, rgb), replace(cx, id, rgb as f32)));
        }
    }

    /// The aux pass's meshes; `ghost`: only the parts drawn as ghosts (the ghost layer's).
    fn draw_aux_pass(&mut self, cx: &mut Cx3d, scene_state: SceneState3D, state: &AppState, ghost: bool) {
        let scene = self.scene().clone();
        let scene = scene.borrow();
        self.draw_aux.draw_vars.set_uniform(cx.cx, live_id!(ghost_ctl), &[if ghost { 1.0 } else { 0.0 }, 0.0, 0.0, 0.0]);
        cx.begin_scene_3d(scene_state);
        let previous = cx.set_scene_world_transform_3d(Mat4f::identity());
        for (i, _batch) in state.scene.batches.iter().enumerate() {
            let Some(geom) = scene.batch_geometries.get(i) else {
                continue;
            };
            let id = geom.geometry_id();
            self.draw_aux.transform = Mat4f::identity();
            self.draw_aux.draw_geometry(cx.cx, id);
            self.draw_calls += 1;
        }
        if let Some(previous) = previous {
            let _ = cx.set_scene_world_transform_3d(previous);
        }
        cx.end_scene_3d();
    }

    /// FAB_SDF_PROF=1: the GPU time of whole frames (every pass summed) and
    /// of the coarse pass, medians logged every 60 frames.
    fn sdf_gpu_prof(&mut self, cx: &mut Cx) {
        if !self.sdf_prof_enabled {
            return;
        }
        fab_sdf_tiles::trace::LOG_PASSES.store(true, std::sync::atomic::Ordering::Relaxed);
        // An answer is the whole recording kept (the HAL encodes it, this decodes it): asked
        // for every frame, that churned over a GB a second of decoded frames. Asked for once a
        // second, a recording kept three seconds still holds every frame since the last
        // answer.
        let now = Cx::time_now();
        if now - self.sdf_prof_asked < 1.0 {
            return;
        }
        self.sdf_prof_asked = now;
        let Some(rec) = cx.gpu_prof_recording(3) else { return };
        for f in rec.frames.iter().filter(|f| f.id > self.sdf_prof_from && !f.split) {
            let total: u64 = f.passes.iter().filter(|p| p.end > p.start).map(|p| p.end - p.start).sum();
            if total > 0 {
                let ms = total as f64 / 1.0e6;
                self.sdf_prof.push(ms);
                let mut frame: Vec<(&str, f64)> = Vec::new();
                for p in f.passes.iter().filter(|p| p.end > p.start) {
                    let (name, d) = (rec.s(p.name), (p.end - p.start) as f64 / 1.0e6);
                    match frame.iter_mut().find(|(n, _)| *n == name) {
                        Some(e) => e.1 += d,
                        None => frame.push((name, d)),
                    }
                }
                for (name, d) in frame {
                    match self.sdf_prof_passes.iter_mut().find(|(n, _)| n == name) {
                        Some(e) => e.1.push(d),
                        None => self.sdf_prof_passes.push((name.to_string(), vec![d])),
                    }
                }
                if self.sdf_prof_slowest.as_ref().is_none_or(|(worst, _)| ms > *worst) {
                    let mut passes: Vec<(String, f64)> = Vec::new();
                    for p in f.passes.iter().filter(|p| p.end > p.start) {
                        let name = format!("{} {}x{}", rec.s(p.name), p.w, p.h);
                        let d = (p.end - p.start) as f64 / 1.0e6;
                        match passes.iter_mut().find(|(n, _)| *n == name) {
                            Some(e) => e.1 += d,
                            None => passes.push((name, d)),
                        }
                    }
                    passes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                    self.sdf_prof_slowest = Some((ms, passes));
                }
            }
        }
        self.sdf_prof_from = rec.frames.last().map_or(self.sdf_prof_from, |f| f.id);
        if self.sdf_prof.len() >= 60 {
            let mut v = std::mem::take(&mut self.sdf_prof);
            v.sort_by(|a, b| a.partial_cmp(b).unwrap());
            log!("fab vp sdf gpu: frame median {:.2} ms, p90 {:.2} ms (n={})", v[v.len() / 2], v[v.len() * 9 / 10], v.len());
            // over every frame of the window: a frame the pass did not run in counts 0
            let frames = v.len();
            let mut passes: Vec<(String, f64)> = std::mem::take(&mut self.sdf_prof_passes)
                .into_iter()
                .map(|(n, mut t)| {
                    t.resize(frames.max(t.len()), 0.0);
                    t.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    (n, t[t.len() / 2])
                })
                .collect();
            passes.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
            let list: Vec<String> = passes.iter().take(8).map(|(n, d)| format!("{n} {d:.3}")).collect();
            log!("fab vp sdf gpu: pass medians: {}", list.join(", "));
            if let Some((ms, passes)) = self.sdf_prof_slowest.take() {
                let top: Vec<String> = passes.iter().take(5).map(|(n, d)| format!("{n} {d:.2}")).collect();
                log!("fab vp sdf gpu: slowest frame {ms:.2} ms: {}", top.join(", "));
            }
        }
    }

    /// The project's picture or the rig's grab was not asked in this draw: draw again when what
    /// it waits for is there, never every frame while a shader compiles.
    fn picture_wait(&mut self, cx: &mut Cx, wait: picture::DrawWait) {
        match wait {
            picture::DrawWait::Frame => {
                self.picture_waits_frame = true;
                self.next_frame = cx.new_next_frame();
            }
            picture::DrawWait::Pipeline => self.picture_waits_shader = true,
        }
    }

    /// Nothing is moving, tracing or settling: the
    /// still view's average may take its next sample.
    /// Another pane's navigator moves the camera this pane shares: a still sample now would be
    /// thrown away by its next move. The panes draw again when it stops.
    fn camera_moved_elsewhere(&self) -> bool {
        let Some(scene) = self.scene.as_ref() else { return false };
        let scene = scene.borrow();
        self.shares_camera && scene.camera_moving && scene.navigation_pane != Some(self.pane)
    }

    /// The navigation owner says whether it moves the shared camera, for the panes that share it
    /// ([`Self::camera_moved_elsewhere`]); when it stops, every view draws again to take its samples.
    fn publish_motion(&self, cx: &mut Cx) {
        let moving = self.shares_camera && self.navigator.is_animating();
        let Some(scene) = self.scene.as_ref() else { return };
        let mut scene = scene.borrow_mut();
        if scene.navigation_pane != Some(self.pane) || scene.camera_moving == moving {
            return;
        }
        scene.camera_moving = moving;
        drop(scene);
        if !moving {
            cx.redraw_all();
        }
    }

    /// The owner stops moving the shared camera without its navigator saying so (parked).
    fn publish_still(&self, cx: &mut Cx) {
        let Some(scene) = self.scene.as_ref() else { return };
        let mut scene = scene.borrow_mut();
        if scene.navigation_pane == Some(self.pane) && scene.camera_moving {
            scene.camera_moving = false;
            drop(scene);
            cx.redraw_all();
        }
    }

    fn still_settled(&self) -> bool {
        if self.camera_moved_elsewhere() {
            return false;
        }
        let scene = self.scene().clone();
        let scene = scene.borrow();
        !self.force_record
            && !self.ssao_waiting
            && self.items_wait.is_none()
            && self.issue_spot_drawn.is_none()
            && !scene.sdf_busy
            && !scene.lighting_pending()
            && !scene.board_layer.building()
            && !self.navigator.is_animating()
            && self.debug_view == 0.0
            && !self.sdf_prof_enabled
            && !self.head_moves
            && !self.edit_moving
    }

    fn draw_lit_pass(&mut self, cx: &mut Cx3d, scene_state: SceneState3D, state: &AppState, world: &makepad_scene::World) {
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        let scene = &mut *scene;
        let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
        let realtime = state.view.shading == Shading::Realtime;
        renderer.set_pbr_materials_enabled(realtime);
        let shadows = if self.debug_off as u32 & 64 != 0 { GpuLightmapMode::Off } else { GpuLightmapMode::Realtime };
        renderer.set_output_dither(self.debug_off as u32 & 512 == 0);
        if renderer.gpu_lightmap_mode() != shadows {
            renderer.set_gpu_lightmap_mode(shadows);
        }
        let camera = state.view.camera(self.pane);
        // The table already fitted opaque parts where they show, including moved fields.
        // Glass and ghosts contribute neither caster bounds nor moved mesh reach.
        let (mut shown, meshes_moved) = self.elem_lut.shown_bounds();
        if let Some(board) = scene.board_layer.bounds(&self.board_moves) {
            shown = aabb_union(&shown, &Aabb { min: board.min * 0.001, max: board.max * 0.001 });
        }
        let to_sun = renderer.frame_sun_dir(world);
        scene.sdf.borrow_mut().set_casters(state, &render_from_fab(), [to_sun.x, to_sun.y, to_sun.z]);
        scene.board_layer.casters.borrow_mut().set_clip_planes(&sdf::planes_in_world(view_planes(state).as_slice(), &render_from_fab()));
        scene.board_layer.casters.borrow_mut().set_moves(&self.board_moves);
        renderer.set_csm_scene_bounds(render_scene_bounds(&shown));
        // Mesh parts moved through the element lookup cast from where they show.
        renderer.set_element_reach(render_scene_bounds(&meshes_moved));
        renderer.set_csm_config(
            DEFAULT_CSM_CONFIG.tile_resolution,
            csm_far_range(&shown, camera),
        );
        renderer.set_csm_focus_distance(Some(camera.distance()));
        renderer.set_sky_time(state.sun.time_local);
        // SceneGpu installed the shared instance with the upload. Both cameras resolve
        // this frame's lights from `world`, without copying the host camera's frame.
        let mut draws = SceneDraws {
            cube: None,
            alpha: None,
            sky: &mut self.draw_sky,
            sky_analytic: Some(&mut self.draw_sky_analytic),
            terrain: None,
            // the shadows the renderer lays on the ground: none from under the bed
            shadow: if state.floor_shown_in(self.pane) { Some(&mut self.draw_shadow) } else { None },
            shadow_sdf: None,
            firework: None,
            flare: None,
            water: None,
            screen: None,
            screen_instances: &[],
            view_model: None,
        };
        // Material draw handles stay on the host outside this secondary camera's encoding.
        if scene.host != Some(self.pane) {
            self.review.as_deref_mut().unwrap().swap_review_materials(&mut scene.renderer);
        }
        let stats = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).draw_scene_full(
            cx,
            &mut self.lit_list,
            &mut draws,
            world,
            scene_state,
            None,
            Some(&mut self.draw_models),
        );
        if scene.host != Some(self.pane) {
            self.review.as_deref_mut().unwrap().swap_review_materials(&mut scene.renderer);
        }
        let _ = stats;
    }

    /// The rebuilding sweep's uniforms: the part's element, its strength (ramping in while the
    /// edit is in flight, fading out over 0.4 s when the rebuilt part lands), seconds since it
    /// started, and its screen span with the pass's pixel height. `None` while it is off. Asks
    /// for the next frame only while it is on.
    fn rebuild_uniforms(&mut self, cx: &mut Cx2d, rect: Rect, state: &AppState) -> Option<([f32; 4], [f32; 4])> {
        let now = Cx::time_now();
        match &state.params.rebuilding {
            Some(r) => {
                if self.rebuild_fx.as_ref().is_none_or(|f| f.object != r.object || f.ended.is_some()) {
                    self.rebuild_fx = Some(RebuildFx { object: r.object.clone(), started: now, ended: None });
                }
            }
            None => {
                if let Some(f) = &mut self.rebuild_fx {
                    if f.ended.is_none() {
                        f.ended = Some(now);
                    }
                }
            }
        }
        let f = self.rebuild_fx.as_ref()?;
        let base = 1.0;
        let ramp = ((now - f.started) / 0.15).clamp(0.0, 1.0);
        let fade = f.ended.map_or(1.0, |e| 1.0 - ((now - e) / 0.4).clamp(0.0, 1.0));
        let strength = (base * ramp * fade) as f32;
        if fade <= 0.0 {
            self.rebuild_fx = None;
            return None;
        }
        let debug = self.param_log && (now - f.started) < 0.05;
        let Some(part) = state.plate.parts.iter().find(|p| p.object == f.object) else {
            if debug { log!("fab vp rebuild effect: no part {}", f.object); }
            return None;
        };
        let Some(element) = part.element else {
            if debug { log!("fab vp rebuild effect: part {} has no element", f.object); }
            return None;
        };
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        let (mut top, mut bottom) = (f64::MAX, f64::MIN);
        for c in 0..8 {
            let p = [
                if c & 1 == 0 { part.min_mm[0] } else { part.max_mm[0] },
                if c & 2 == 0 { part.min_mm[1] } else { part.max_mm[1] },
                if c & 4 == 0 { part.min_mm[2] } else { part.max_mm[2] },
            ];
            if let Some(s) = proj.project(vec3(p[0] as f32 * 0.001, p[1] as f32 * 0.001, p[2] as f32 * 0.001)) {
                let v = (s.y - rect.pos.y) / rect.size.y.max(1.0);
                top = top.min(v);
                bottom = bottom.max(v);
            }
        }
        if top > bottom {
            if debug { log!("fab vp rebuild effect: part {} off screen", f.object); }
            return None;
        }
        self.next_frame = cx.new_next_frame();
        if debug {
            log!("fab vp rebuild effect: element {} strength {strength:.2} span {top:.3}..{bottom:.3}", element.0);
        }
        let px_h = (rect.size.y * cx.current_dpi_factor()).max(1.0) as f32;
        Some(([element.0 as f32 + 1.0, strength, (now - f.started) as f32, 0.0], [top as f32, bottom as f32, px_h, 0.0]))
    }

    fn draw_comp_pass(&mut self, cx: &mut Cx2d, rect: Rect, state: &AppState, fab_state: SceneState3D, light: &TileLight, planes: &[[f32; 4]]) {
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        let scene = &mut *scene;
        let vs = &state.view;
        let dpi = cx.current_dpi_factor() as f32;
        let px = vec2(
            (rect.size.x as f32 * dpi).max(1.0),
            (rect.size.y as f32 * dpi).max(1.0),
        );
        let realtime = if vs.shading == Shading::Realtime { 1.0 } else { 0.0 };
        let section_on = caps_shown(state, planes);
        // the part the composite's key tints (`hover_tint`)
        let hover = self.comp_key
            .and_then(|(element, _)| element)
            .map(|element| element.0 as f32 + 1.0)
            .unwrap_or(0.0);
        let (lw, lh) = self.elem_lut.size();
        let lut_on = if self.elem_lut.texture().is_some() { 1.0 } else { 0.0 };
        let flags = shading_overlay_flags(vs);

        // the recording's own camera (a still view's sample is jittered)
        let (fab_inv_proj, fab_inv_view) = (fab_state.projection.invert(), fab_state.view.invert());
        if scene.plate_index.is_none_or(|(generation, _)| generation != state.scene.generation) {
            let index = state.scene.elements.iter()
                .find(|e| e.name.strip_prefix(CONTEXT_PREFIX) == Some("plate-face"))
                .map_or(-1.0, |e| e.id.0 as f32);
            scene.plate_index = Some((state.scene.generation, index));
        }
        let luminance = |c: [f32; 3]| c[0] * 0.2126 + c[1] * 0.7152 + c[2] * 0.0722;
        let comp = &mut self.draw_comp.draw_super.draw_vars;
        comp.set_texture(0, &self.lit_texture);
        comp.set_texture(1, &self.aux_texture);
        if let Some(t) = self.elem_lut.texture() {
            comp.set_texture(2, t);
        }
        comp.set_uniform(
            cx.cx,
            live_id!(u_mode),
            &[
                realtime,
                if vs.xray { 1.0 } else { 0.0 },
                if section_on { 1.0 } else { 0.0 },
                if vs.lighting_for(&state.ui).is_studio() { 1.0 } else { 0.0 },
            ],
        );
        comp.set_uniform(cx.cx, live_id!(u_flags), &[flags[0], if self.debug_off as u32 & 128 == 0 { flags[1] } else { 0.0 }, if self.debug_off as u32 & 8 != 0 { 1.0 } else { 0.0 }, realtime_edge_light(vs)]);
        comp.set_uniform(cx.cx, live_id!(u_hover), &[hover, 0.0, 0.0, 0.0]);
        // about 2 px at the display's scale; searched for only while something can be selected
        let can_select = !state.scene_state.selection.is_empty() || state.print.play.active || !state.elec_selected.is_empty();
        comp.set_uniform(cx.cx, live_id!(u_ring), &[(2.0 * rig_mark_dpi(dpi)).clamp(1.0, 4.5), if can_select { 1.0 } else { 0.0 }, 0.0, 0.0]);
        comp.set_uniform(
            cx.cx,
            live_id!(u_texel),
            &[1.0 / px.x, 1.0 / px.y, px.x, px.y],
        );
        comp.set_uniform(cx.cx, live_id!(elem_ctl), &[lw, lh, STRIDE as f32, lut_on]);
        let ssao = self.ssao_texture.as_ref().filter(|_| self.ssao_on);
        if let Some(shader) = comp.draw_shader_id {
            let slot_of = |id: LiveId| cx.cx.draw_shaders[shader.index].mapping.textures.iter().position(|t| t.id == id);
            if let Some(slot) = slot_of(live_id!(ghost_aux)) {
                comp.set_texture(slot, if self.ghost_on { &self.ghost_texture } else { &self.aux_texture });
            }
            if let Some(slot) = slot_of(live_id!(ssao_tex)) {
                comp.set_texture(slot, ssao.unwrap_or(&self.aux_texture));
            }
        }
        // the screen AO's two terms, as the renderer's model lanes weigh an AO
        let ao = if ssao.is_some() { [PLATE_AO_STRENGTH, ao_direct_weight(self.hdr)] } else { [0.0; 2] };
        comp.set_uniform(cx.cx, live_id!(u_ao), &[ao[0], ao[1], 0.0, 0.0]);
        comp.set_uniform(cx.cx, live_id!(u_ghost), &[if self.ghost_on { 1.0 } else { 0.0 }, AppState::GHOST_ALPHA, GHOST_TIE, AppState::ARRIVING_ALPHA]);
        Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).bind_sun_cascades(cx.cx, comp);
        comp.set_uniform(cx.cx, live_id!(u_inv_proj), &fab_inv_proj.v);
        comp.set_uniform(cx.cx, live_id!(u_inv_view), &fab_inv_view.v);
        comp.set_uniform(cx.cx, live_id!(u_sun_dir), &light.dir);
        comp.set_uniform(cx.cx, live_id!(u_key), &[luminance(light.color), (luminance(light.sky) + luminance(light.ground)) * 0.5, 0.0, 0.0]);
        comp.set_uniform(cx.cx, live_id!(u_plate), &[scene.plate_index.unwrap().1, PLATE_LINE_MINOR, PLATE_LINE_MAJOR, 0.0]);
        comp.set_uniform(cx.cx, live_id!(u_plate_rect), &[
            (state.plate.bed_min[0] * 0.001) as f32,
            (state.plate.bed_min[1] * 0.001) as f32,
            (state.plate.bed_max[0] * 0.001) as f32,
            (state.plate.bed_max[1] * 0.001) as f32,
        ]);

        let pass_rect = Rect {
            pos: dvec2(0.0, 0.0),
            size: rect.size,
        };
        self.draw_comp.draw_super.draw_abs(cx, pass_rect);
        self.draw_calls += 1;
    }

    /// Over the composite: the ground grid and the axis lines (Solid's drafting scaffolding;
    /// Realtime shows the renderer's own world and the plate scene's bed), the build volume's
    /// edges in every shading, a shown resin layer's image, and over a shown print the model's
    /// outline at its top layer (below the print's last, `print::outline_at`) and the ring round
    /// the printing nozzle's tip.
    /// `px`: the pass's size in pixels and its density.
    fn draw_comp_overlays(&mut self, cx: &mut Cx3d, scene_state: SceneState3D, state: &AppState, px: [f32; 3], print_frame: Option<&print::PrintFrame>) {
        // The printing nozzle's tip, ringed on the bead.
        let ring = print_frame.and_then(|f| f.heads.iter().find(|h| h.active)).map(|h| h.tip);
        let vs = &state.view;
        let solid = vs.shading == Shading::Solid;
        // the grid and axes lie on the floor: not from under the bed
        let show_grid = solid && vs.overlays.grid && state.floor_shown_in(self.pane);
        let show_axes = solid && vs.overlays.axes && state.floor_shown_in(self.pane);
        let volume = Volume::of(state);
        // rig: `nooutline` draws no layer outline (the skins' statistics of the 9x reference)
        let outline = print::outline_at(state, print_frame).filter(|_| !rig_mode().split_whitespace().any(|w| w == "nooutline"));
        let resin = state.resin_shown().and_then(|r| Some((r, r.image.as_ref().filter(|im| !im.is_empty())?, r.texture.as_ref()?)));
        let picks = pick_lines(state, self.pane);
        let no_picks = picks.iter().all(|g| g.is_empty());
        if no_picks {
            self.pick_geometry = Default::default();
        }
        if !show_grid && !show_axes && volume.is_none() && outline.is_none() && resin.is_none() && ring.is_none() && no_picks {
            return;
        }
        let radius = if aabb_is_empty(&state.scene.bounds) {
            50.0
        } else {
            aabb_radius(&state.scene.bounds).max(4.0)
        };
        cx.begin_scene_3d(scene_state);
        let previous = cx.set_scene_world_transform_3d(Mat4f::identity());
        if show_grid || show_axes {
            let half = (radius * 60.0).clamp(500.0, 20000.0);
            if self.ground_geometry.is_none() {
                self.ground_geometry = Some(ground_quad(cx.cx, half));
            }
            let gid = self.ground_geometry.as_ref().unwrap().geometry_id();
            self.draw_grid.draw_vars.set_texture(0, &self.aux_texture);
            self.draw_grid.draw_vars.set_uniform(
                cx.cx,
                live_id!(u_grid_flags),
                &[
                    if show_axes { 1.0 } else { 0.0 },
                    if show_grid { 1.0 } else { 0.0 },
                    0.0,
                    0.0,
                ],
            );
            self.draw_grid.transform = Mat4f::identity();
            self.draw_grid.fade_far = (radius * 14.0).max(120.0);
            let c = state.view.camera(self.pane);
            self.draw_grid.ortho_reach = if c.ortho { crate::nav::orbit::seen_from(c) } else { 0.0 };
            self.draw_grid.draw_geometry(cx.cx, gid);
            self.draw_calls += 1;
        }
        if let Some(volume) = volume {
            if self.volume_geometry.as_ref().map(|(v, _)| *v) != Some(volume) {
                self.volume_geometry = Some((volume, volume.lines(cx.cx)));
            }
            let gid = self.volume_geometry.as_ref().unwrap().1.geometry_id();
            self.draw_volume.draw_vars.set_texture(0, &self.aux_texture);
            // A line one point wide.
            self.draw_volume.draw_vars.set_uniform(cx.cx, live_id!(u_px), &[px[0], px[1], 0.5 * px[2], 0.0]);
            self.draw_volume.draw_geometry(cx.cx, gid);
            self.draw_calls += 1;
        }
        if let Some((show, image, texture)) = resin {
            let pitch = show.pitch.map(f64::to_bits);
            if self.resin_quad.as_ref().is_none_or(|q| (q.0, q.1) != (image.rect, pitch)) {
                self.resin_quad = Some((image.rect, pitch, resin_quad(cx.cx, image.rect, show.pitch)));
            }
            let gid = self.resin_quad.as_ref().unwrap().2.geometry_id();
            self.draw_resin_layer.lift = crate::tools::transform::mm_to_world([0.0, 0.0, show.top_of(image.layer)]).z;
            let vars = &mut self.draw_resin_layer.draw_vars;
            set_texture_named(vars, cx.cx, live_id!(aux), &self.aux_texture);
            set_texture_named(vars, cx.cx, live_id!(layer), texture);
            self.draw_resin_layer.draw_geometry(cx.cx, gid);
            self.draw_calls += 1;
        }
        // The model's outline at the shown layer: a thin line in the overlay colour, not a bead,
        // so it never reads as a wall. Each segment is capped past its ends, so the outline is
        // one line through every joint.
        if let Some((live, layer)) = outline {
            let ids: Vec<u64> = live.contours.layer(layer).map(|b| b.id()).collect();
            if self.outline_geometry.as_ref().is_none_or(|(l, i, _)| *l != layer || *i != ids) {
                let mut edges = Vec::new();
                for edge_list in live.contours.layer(layer) {
                    for seg in edge_list.segments() {
                        for e in seg.data().chunks_exact(edge_list.slots()) {
                            let at = |i: usize| crate::tools::transform::mm_to_world([e[i] as f64, e[i + 1] as f64, e[i + 2] as f64]);
                            edges.push((at(0), at(4)));
                        }
                    }
                }
                self.outline_geometry = Some((layer, ids, line_geometry(cx.cx, edges.into_iter())));
            }
            let gid = self.outline_geometry.as_ref().unwrap().2.geometry_id();
            self.draw_outline.draw_vars.set_texture(0, &self.aux_texture);
            // A line a point and a half wide.
            self.draw_outline.draw_vars.set_uniform(cx.cx, live_id!(u_px), &[px[0], px[1], 0.75 * px[2], 1.0]);
            self.draw_outline.draw_geometry(cx.cx, gid);
            self.draw_calls += 1;
        }
        if let Some(tip) = ring {
            if self.head_ring.as_ref().is_none_or(|(at, _)| *at != tip) {
                // a hair over the layer it prints, so the bead's top never hides it
                let point = |k: usize| {
                    let a = k as f32 * std::f32::consts::TAU / 32.0;
                    crate::tools::transform::mm_to_world([(tip[0] + print::HEAD_RING_MM * a.cos()) as f64, (tip[1] + print::HEAD_RING_MM * a.sin()) as f64, tip[2] as f64 + 0.05])
                };
                let (index, vertices) = line_buffers((0..32).map(|k| (point(k), point(k + 1))));
                match &mut self.head_ring {
                    Some((at, g)) => {
                        *at = tip;
                        g.update(cx.cx, index, vertices);
                    }
                    None => {
                        let g = Geometry::new(cx.cx);
                        g.update(cx.cx, index, vertices);
                        self.head_ring = Some((tip, g));
                    }
                }
            }
            let gid = self.head_ring.as_ref().unwrap().1.geometry_id();
            self.draw_head_ring.draw_vars.set_texture(0, &self.aux_texture);
            // A line two points wide, one through its joints.
            self.draw_head_ring.draw_vars.set_uniform(cx.cx, live_id!(u_px), &[px[0], px[1], px[2], 1.0]);
            self.draw_head_ring.draw_geometry(cx.cx, gid);
            self.draw_calls += 1;
        }
        // The picks' outlines, over the parts: a run behind something dimmed and dashed. Their
        // geometry is made again only when a pick, the pointer's answer or a placement moves it.
        for (group, lines) in picks.iter().enumerate() {
            if lines.is_empty() {
                self.pick_geometry[group] = None;
                continue;
            }
            let (index, vertices) = pick_line_buffers(lines);
            if self.pick_geometry[group].as_ref().is_none_or(|(v, _)| *v != vertices) {
                let g = self.pick_geometry[group].take().map_or_else(|| Geometry::new(cx.cx), |(_, g)| g);
                g.update(cx.cx, index, vertices.clone());
                self.pick_geometry[group] = Some((vertices, g));
            }
            let gid = self.pick_geometry[group].as_ref().unwrap().1.geometry_id();
            // points wide: what a click would take, a pick, the pick whose chip is lit
            let (draw, width) = match group {
                0 => (&mut self.draw_pick_under, 1.4),
                1 => (&mut self.draw_pick_lines, 2.0),
                _ => (&mut self.draw_pick_lit, 2.6),
            };
            draw.draw_vars.set_texture(0, &self.aux_texture);
            draw.draw_vars.set_uniform(cx.cx, live_id!(u_px), &[px[0], px[1], 0.5 * width * px[2], px[2]]);
            draw.draw_vars.set_uniform(cx.cx, live_id!(u_hidden), &[7.0 * px[2], 0.5, 0.0, 0.0]);
            draw.draw_geometry(cx.cx, gid);
            self.draw_calls += 1;
        }
        if let Some(previous) = previous {
            let _ = cx.set_scene_world_transform_3d(previous);
        }
        cx.end_scene_3d();
    }

    /// See-Through's parts over the composite: each grid's box, marched to what `aux` holds.
    fn draw_see_through(&mut self, cx: &mut Cx3d, scene_state: SceneState3D, state: &AppState, px: [f32; 3]) {
        let scene = self.scene().clone();
        let scene = scene.borrow();
        let (Some(atlas), Some(occ), Some(index)) = (scene.see.atlas(), scene.see.occ_atlas(), scene.see.index()) else { return };
        let drawn = scene.see.drawn();
        if drawn.is_empty() || !state.view.see.on {
            return;
        }
        if self.see_box.is_none() {
            self.see_box = Some(unit_cube(cx.cx));
        }
        let gid = self.see_box.as_ref().unwrap().geometry_id();
        cx.begin_scene_3d(scene_state);
        let previous = cx.set_scene_world_transform_3d(Mat4f::identity());
        let vars = &mut self.draw_see.draw_vars;
        set_texture_named(vars, cx.cx, live_id!(aux), &self.aux_texture);
        set_texture_named(vars, cx.cx, live_id!(atlas), atlas);
        set_texture_named(vars, cx.cx, live_id!(occ_map), occ);
        set_texture_named(vars, cx.cx, live_id!(index_map), index);
        vars.set_uniform(cx.cx, live_id!(u_see), &[state.view.see.opacity, if state.view.camera(self.pane).ortho { 1.0 } else { 0.0 }, px[0], px[1]]);
        vars.set_uniform(cx.cx, live_id!(u_glow), &[state.view.see.glow, 0.0, 0.0, 0.0]);
        vars.set_uniform(cx.cx, live_id!(u_sizes), &scene.see.sizes());
        for d in drawn {
            self.draw_see.world_from_lattice = d.world_from_lattice;
            let [x, y, z] = d.lattice_from_world;
            self.draw_see.lat_x = Vec4f { x: x[0], y: x[1], z: x[2], w: x[3] };
            self.draw_see.lat_y = Vec4f { x: y[0], y: y[1], z: y[2], w: y[3] };
            self.draw_see.lat_z = Vec4f { x: z[0], y: z[1], z: z[2], w: z[3] };
            self.draw_see.bricks = Vec4f { x: d.grid[0], y: d.grid[1], z: d.grid[2], w: d.grid[3] };
            self.draw_see.draw_geometry(cx.cx, gid);
            self.draw_calls += 1;
        }
        if let Some(previous) = previous {
            let _ = cx.set_scene_world_transform_3d(previous);
        }
        cx.end_scene_3d();
    }

    /// See-Through's march at half the composite's pixels each way into `see_texture`, for
    /// [`Self::draw_see_up`]: false when there is nothing to march.
    fn record_see_pass(&mut self, cx: &mut Cx2d, rect: Rect, state: &AppState, fab_state: SceneState3D) -> bool {
        let scene = self.scene().clone();
        let scene = scene.borrow();
        if !state.view.see.on || scene.see.drawn().is_empty() {
            return false;
        }
        drop(scene);
        let dpi = cx.current_dpi_factor() * 0.5;
        self.see_pass.set_pass_parent(cx.cx, self.comp_pass.draw_pass_id());
        self.see_pass.set_size(cx, rect.size);
        self.see_pass.set_color_texture(cx, &self.see_texture, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
        cx.begin_pass(&self.see_pass, Some(dpi));
        self.see_pass.set_size(cx, rect.size);
        self.see_pass.set_camera(cx.cx, fab_state.view, fab_state.projection);
        // a draw list first: see `record_comp_pass`
        self.see_list.begin_always(cx);
        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        let px = [(rect.size.x * dpi).floor() as f32, (rect.size.y * dpi).floor() as f32, rig_mark_dpi(dpi as f32)];
        self.draw_see_through(&mut Cx3d::new(cx.cx), fab_state, state, px);
        cx.end_pass_sized_turtle();
        self.see_list.end(cx);
        cx.end_pass(&self.see_pass);
        true
    }

    /// The half-resolution march ([`Self::record_see_pass`]) over the composite.
    fn draw_see_up(&mut self, cx: &mut Cx2d, rect: Rect) {
        let dpi = cx.current_dpi_factor() * 0.5;
        let vars = &mut self.draw_see_up.draw_super.draw_vars;
        set_texture_named(vars, cx.cx, live_id!(see_map), &self.see_texture);
        set_texture_named(vars, cx.cx, live_id!(aux), &self.aux_texture);
        vars.set_uniform(cx.cx, live_id!(u_half), &[(rect.size.x * dpi).floor().max(1.0) as f32, (rect.size.y * dpi).floor().max(1.0) as f32, 0.0, 0.0]);
        self.draw_see_up.draw_super.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: rect.size });
        self.draw_calls += 1;
    }

    /// The display curve's values (`FabDisplay`: the exposure, the HDR lane, the contrast, the
    /// dither switch; the white balance) on every draw that takes it, before the aux pass draws:
    /// the far aux puts its beads' taps on the display with them, the composite the scene's light.
    fn bind_display(&mut self, cx: &mut Cx, vs: &ViewportState, lighting: Lighting) {
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        let scene = &mut *scene;
        let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
        let (exposure, balance, contrast) = display_transform(renderer, vs, lighting, self.hdr);
        let exposure = [exposure, if self.hdr { 1.0 } else { 0.0 }, contrast, if self.debug_off as u32 & 512 != 0 { 1.0 } else { 0.0 }];
        for vars in [&mut self.draw_comp.draw_super.draw_vars, &mut self.draw_glow_over.draw_super.draw_vars, &mut self.draw_far_aux.draw_vars, &mut self.draw_glass.draw_vars] {
            vars.set_uniform(cx, live_id!(u_exposure), &exposure);
            vars.set_uniform(cx, live_id!(u_balance), &balance);
        }
    }

    /// Both the aux pass and `libs/render`'s static lane learn about hidden
    /// elements, drag offsets and section planes here — the single place the
    /// two can be kept in step.
    fn bind_state_uniforms(&mut self, cx: &mut Cx, planes: &[[f32; 4]]) {
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        let scene = &mut *scene;
        scene.board_layer.set_clip_planes(planes);
        let (lw, lh) = self.elem_lut.size();
        let on = if self.elem_lut.texture().is_some() && lw > 0.0 {
            1.0
        } else {
            0.0
        };
        let count = planes.len() as f32;

        if let Some(t) = self.elem_lut.texture() {
            self.draw_aux.draw_vars.set_texture(0, t);
            self.draw_models.draw_vars.set_texture(6, t);
        }
        // The renderer's own PBR lane (Realtime with textured materials) takes
        // the same lookup, or a hidden or dragged part would stay put there.
        Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).set_element_lookup(self.elem_lut.texture().cloned().map(|t| (t, [lw, lh, STRIDE as f32, on])), self.elem_lut.casters());
        self.draw_aux
            .draw_vars
            .set_uniform(cx, live_id!(elem_ctl), &[lw, lh, STRIDE as f32, on]);
        self.draw_models
            .draw_vars
            .set_uniform(cx, live_id!(elem_ctl), &[lw, lh, STRIDE as f32, on]);

        self.draw_aux
            .draw_vars
            .set_uniform(cx, live_id!(clip_ctl), &[count, 0.0, 0.0, 0.0]);
        self.draw_models
            .draw_vars
            .set_uniform(cx, live_id!(clip_ctl), &[count, 0.0, 0.0, 0.0]);
        const NAMES: [LiveId; 6] = [
            live_id!(clip0),
            live_id!(clip1),
            live_id!(clip2),
            live_id!(clip3),
            live_id!(clip4),
            live_id!(clip5),
        ];
        for i in 0..6 {
            let p = planes.get(i).copied().unwrap_or([0.0, 0.0, 0.0, 0.0]);
            self.draw_aux.draw_vars.set_uniform(cx, NAMES[i], &p);
            // The lit model lives in the Y-up turn of the same world, so the
            // plane takes the same turn.
            let n = pack::to_render(vec3(p[0], p[1], p[2]));
            self.draw_models
                .draw_vars
                .set_uniform(cx, NAMES[i], &[n.x, n.y, n.z, p[3]]);
        }
    }

    /// The resin cut every draw that clips takes (`clip_cut`): the meshes and tiles of the aux
    /// passes in Fab's frame, the lit tiles and the lit meshes (the renderer's clip plane) in the
    /// renderer's.
    fn bind_cut(&mut self, cx: &mut Cx, cut: Option<[f32; 4]>) {
        let (fab, render) = cut_planes(cut);
        self.draw_aux.draw_vars.set_uniform(cx, live_id!(clip_cut), &fab);
        self.sdf_tile.draw_vars.set_uniform(cx, live_id!(clip_cut), &fab);
        self.sdf_tile_lit.draw_vars.set_uniform(cx, live_id!(clip_cut), &render);
        self.draw_board_aux.draw_vars.set_uniform(cx, live_id!(clip_cut), &fab);
        self.draw_glass.draw_vars.set_uniform(cx, live_id!(clip_cut), &fab);
        self.draw_board.draw_vars.set_uniform(cx, live_id!(clip_cut), &render);
        self.draw_body_aux.draw_vars.set_uniform(cx, live_id!(clip_cut), &fab);
        self.draw_body.draw_vars.set_uniform(cx, live_id!(clip_cut), &render);
        {
            let scene = self.scene().clone();
            let mut scene = scene.borrow_mut();
            let scene = &mut *scene;
            Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).set_clip_plane(cut.map(|_| render));
        }
        self.cut_drawn = cut;
    }

    /// Moves the resin cut in the retained scene passes: each clipping call takes the new plane
    /// in place and the passes paint again; nothing records (a new layer while scrubbing).
    fn move_cut(&mut self, cx: &mut Cx, cut: Option<[f32; 4]>) {
        let (fab, render) = cut_planes(cut);
        let list = |l: &DrawList| Area::Rect(RectArea { draw_list_id: l.id(), rect_id: 0, redraw_id: 0 });
        for l in [&self.aux_list, &self.ghost_list] {
            self.draw_aux.draw_vars.set_uniform_on_draw_list(cx, list(l), live_id!(clip_cut), &fab);
            self.sdf_tile.draw_vars.set_uniform_on_draw_list(cx, list(l), live_id!(clip_cut), &fab);
        }
        self.sdf_tile_lit.draw_vars.set_uniform_on_draw_list(cx, list(&self.print_list), live_id!(clip_cut), &render);
        self.draw_board_aux.draw_vars.set_uniform_on_draw_list(cx, list(&self.aux_list), live_id!(clip_cut), &fab);
        self.draw_glass.draw_vars.set_uniform_on_draw_list(cx, list(&self.comp_list), live_id!(clip_cut), &fab);
        self.draw_board.draw_vars.set_uniform_on_draw_list(cx, list(&self.print_list), live_id!(clip_cut), &render);
        self.draw_body_aux.draw_vars.set_uniform_on_draw_list(cx, list(&self.aux_list), live_id!(clip_cut), &fab);
        self.draw_body.draw_vars.set_uniform_on_draw_list(cx, list(&self.print_list), live_id!(clip_cut), &render);
        if cut.is_some() {
            let scene = self.scene().clone();
            let mut scene = scene.borrow_mut();
            let scene = &mut *scene;
            Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).move_clip_plane(cx, self.lit_list.id(), render);
        }
        // what reads the cut paints again: aux, and each pass up to the composite (the screen
        // AO and lit), and the ghosts; not the sun's cascades, which the cut never reaches
        let (mut pass, comp) = (self.aux_pass.draw_pass_id(), self.comp_pass.draw_pass_id());
        loop {
            cx.repaint_pass(pass);
            match cx.passes[pass].parent.clone() {
                CxDrawPassParent::DrawPass(up) if up != comp => pass = up,
                _ => break,
            }
        }
        if self.ghost_on {
            cx.repaint_pass(self.ghost_pass.draw_pass_id());
        }
        self.cut_drawn = cut;
        self.cuts_moved += 1;
        self.aux_records += 1;
        // the AO's stages paint again over the moved cut as a fresh recording's would: its seed
        // and no history
        self.ssao.forget_history(cx, 0);
        self.ssao_age = 0;
        self.ssao_reset = true;
    }

    /// Records the aux and lit passes (`draw_walk` decides when).
    #[allow(clippy::too_many_arguments)]
    fn record_scene_passes(&mut self, cx: &mut Cx2d, rect: Rect, state: &mut AppState, fab_state: SceneState3D, render_state: SceneState3D, world: &makepad_scene::World, light: TileLight, sdf_drawn: &[ElementId], planes: &[[f32; 4]], cut: Option<[f32; 4]>, print_frame: Option<&print::PrintFrame>, force_frame: Option<&force::ForceFrame>, still_sample: Option<u32>) {
        // Set again where the bodies record their lit instances below; cleared first so a
        // removed board cannot keep requesting live frames.
        self.body_emitting = false;
        let scene_handle = self.scene().clone();
        let scene = scene_handle.borrow();
        scene.sdf.borrow_mut().begin_record();
        drop(scene);
        self.bind_state_uniforms(cx.cx, planes);
        self.bind_display(cx.cx, &state.view, state.view.lighting_for(&state.ui));
        {
            let mut scene = scene_handle.borrow_mut();
            let scene = &mut *scene;
            Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).set_host_box(lit_box(state));
        }
        self.bind_cut(cx.cx, cut);
        let model_markers = if self.role == PaneRole::Plate && print_frame.is_none() && state.plate.whole.is_none() { state.print.markers().cloned() } else { None };

        // Aux supplies depth and normals before the screen AO and lit, then composite and the
        // window. CORTAO parents prepass → main → filter → resolve → lit (no resolve in StillAa);
        // aux parents under its deepest stage, so the G-buffer exists before the taps read it.
        let comp_id = self.comp_pass.draw_pass_id();
        let lit_id = self.lit_pass.draw_pass_id();
        self.lit_pass.set_pass_parent(cx.cx, comp_id);
        let wants_ao = self.debug_off as u32 & 32 == 0 && wants_ssao(&state.view);
        if !wants_ao {
            // Off (Solid, or the debug switch), its passes let go: kept, they would still run
            // with every repaint of lit's tree, over a picture nothing reads.
            self.ssao = CortaoPass::default();
            self.ssao_reset = true;
        }
        if wants_ao && self.ssao.prepass_draw_mut().is_none() {
            if let Some(draw) = cx.cx.try_with_vm(|vm| DrawFabCortaoPrepass::script_new_with_default(vm)) {
                self.ssao.set_prepass_draw(Box::new(draw.draw_super));
            } else {
                self.force_record = true;
                self.next_frame = cx.new_next_frame();
            }
        }
        // Never let ensure install the default hardware-depth hook over Fab's radial aux depth.
        let ssao_ready = wants_ao && self.ssao.prepass_draw_mut().is_some() && self.ssao.ensure(cx.cx);
        // A pipeline that failed to build never answers DrawShadersReady: no AO, no wait.
        self.ssao_waiting = wants_ao && !ssao_ready && !self.ssao.pipelines_failed();
        self.ssao_texture = None;
        // Reparent to the prepass only after run succeeds; otherwise aux still feeds lit.
        self.aux_pass.set_pass_parent(cx.cx, lit_id);

        // ---- aux ----------------------------------------------------------
        self.aux_pass.set_size(cx, rect.size);
        self.aux_pass.set_color_texture(
            cx,
            &self.aux_texture,
            // a = 0 is "nothing here"; the composite paints the background.
            DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)),
        );
        self.aux_pass
            .set_depth_texture(cx, &self.depth_texture, DrawPassClearDepth::ClearWith(1.0));
        cx.begin_pass(&self.aux_pass, None);
        self.aux_pass.set_size(cx, rect.size);
        self.aux_pass.set_camera(cx.cx, fab_state.view, fab_state.projection);
        self.aux_state = Some(fab_state);
        self.aux_records += 1;
        // PASS-LOCAL DRAW LIST + ROOT TURTLE, both load-bearing, in this
        // order (`apps/vj/src/flow_tween.rs`'s stage chain, verbatim):
        // the list because everything drawn before the pass has one lands in
        // the ENCLOSING list — the window's — and gets painted over the UI;
        // the turtle because otherwise every draw inherits the widget's
        // on-screen clip rect, and a viewport that does not start at the
        // window origin has a clip that misses its own offscreen target
        // entirely, so the pass comes out black.
        self.aux_list.begin_always(cx);
        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        {
            let cx3d = &mut Cx3d::new(cx.cx);
            self.draw_aux_pass(cx3d, fab_state, state, false);
            let mut scene = scene_handle.borrow_mut();
            if let Some(frame) = print_frame {
                cx3d.begin_scene_3d(fab_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                let tiers = (&mut self.draw_path_aux, &mut self.draw_far_aux, &mut self.far_tiers, state.view.sliced_detail == SlicedDetail::Exact);
                self.draw_calls += print::draw_print(cx3d, frame, &mut self.print_palette, &light, (&fab_state.view, &fab_state.projection), tiers, &mut self.draw_head_aux);
                // The runs that changed tier, on the recordings they did: none while the camera
                // holds still.
                if let Some((went, came, ranges)) = self.far_tiers.flips() {
                    log!("fab far tier: {} runs went far, {} came near; ranges {} near, {} far", went, came, ranges[0], ranges[1]);
                }
                if let Some(previous) = previous {
                    let _ = cx3d.set_scene_world_transform_3d(previous);
                }
                cx3d.end_scene_3d();
            } else if let Some(markers) = &model_markers {
                cx3d.begin_scene_3d(fab_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                self.draw_calls += print::draw_markers(cx3d, markers, &mut self.print_palette, self.hovered_marker, &mut self.draw_path_aux.draw_vars);
                if let Some(previous) = previous { let _ = cx3d.set_scene_world_transform_3d(previous); }
                cx3d.end_scene_3d();
            }
            if let Some(frame) = force_frame {
                cx3d.begin_scene_3d(fab_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                self.draw_calls += scene.force_layer.draw(cx3d, frame, None, &mut self.draw_force_aux, &mut self.draw_force);
                if let Some(previous) = previous { let _ = cx3d.set_scene_world_transform_3d(previous); }
                cx3d.end_scene_3d();
            }
            if scene.board_layer.select(cx3d.cx, &fab_state, &self.board_moves) {
                cx3d.begin_scene_3d(fab_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                self.draw_calls += scene.board_layer.draw(cx3d, None, &mut self.draw_board_aux, &mut self.draw_board, &self.board_moves);
                self.draw_calls += scene.board_layer.draw_bodies(cx3d, None, None, false, &mut self.draw_body_aux, &mut self.draw_body, &self.board_moves, &state.elec_selected);
                if let Some(previous) = previous { let _ = cx3d.set_scene_world_transform_3d(previous); }
                cx3d.end_scene_3d();
            }
        }
        let scene = scene_handle.borrow();
        self.draw_calls += scene.sdf.borrow_mut().draw(cx, self.pane, sdf::SdfPass::Aux(&mut self.sdf_tile), &fab_state.view, &fab_state.projection, rect.size, Mat4f::identity(), &planes, state);
        cx.end_pass_sized_turtle();
        self.aux_list.end(cx);
        cx.end_pass(&self.aux_pass);
        drop(scene);
        // ---- ghosts -------------------------------------------------------
        // The parts with their eye off or on their way in, alone, into a G-buffer of their own:
        // the composite mixes them in at a quarter, over what the main passes drew. Mesh and
        // field parts alike; they are absent from the lit pass and the cascades, so they cast
        // no shadow.
        let poses = state.pane_poses(self.pane);
        let scene_ghosts = state.ghosts_drawn_under(poses);
        let copy = crate::tools::fit::ghost(state, self.pane);
        self.ghost_on = scene_ghosts || copy.is_some();
        if self.ghost_on {
            self.ghost_pass.set_pass_parent(cx.cx, comp_id);
            self.ghost_pass.set_size(cx, rect.size);
            self.ghost_pass.set_color_texture(cx, &self.ghost_texture, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
            self.ghost_pass.set_depth_texture(cx, &self.ghost_depth, DrawPassClearDepth::ClearWith(1.0));
            cx.begin_pass(&self.ghost_pass, None);
            self.ghost_pass.set_size(cx, rect.size);
            self.ghost_pass.set_camera(cx.cx, fab_state.view, fab_state.projection);
            self.ghost_list.begin_always(cx);
            let pass_size = cx.current_pass_size();
            cx.begin_root_turtle(pass_size, Layout::flow_overlay());
            // (a test print's ghost is the plate it is cut from, whole)
            let ghost_planes = if state.test_cut_shown() { &[][..] } else { planes };
            self.draw_aux.draw_vars.set_uniform(cx.cx, live_id!(clip_ctl), &[ghost_planes.len() as f32, 0.0, 0.0, 0.0]);
            {
                let cx3d = &mut Cx3d::new(cx.cx);
                if scene_ghosts { self.draw_aux_pass(cx3d, fab_state, state, true); }
                if let Some(copy) = copy {
                    // The fitted copy has its own pose baked on the worker. The element lookup
                    // would apply the source's move a second time or hide a field part's mesh.
                    cx3d.begin_scene_3d(fab_state);
                    self.draw_aux.draw_vars.set_uniform(cx3d.cx, live_id!(elem_ctl), &[0.0; 4]);
                    self.draw_aux.draw_geometry(cx3d.cx, copy.geometry);
                    let (w, h) = self.elem_lut.size();
                    self.draw_aux.draw_vars.set_uniform(cx3d.cx, live_id!(elem_ctl), &[w, h, STRIDE as f32, 1.0]);
                    cx3d.end_scene_3d();
                    self.draw_calls += 1;
                }
            }
            let scene = scene_handle.borrow();
            if scene_ghosts {
                self.draw_calls += scene.sdf.borrow_mut().draw(cx, self.pane, sdf::SdfPass::GhostAux(&mut self.sdf_tile), &fab_state.view, &fab_state.projection, rect.size, Mat4f::identity(), ghost_planes, state);
            }
            cx.end_pass_sized_turtle();
            self.ghost_list.end(cx);
            cx.end_pass(&self.ghost_pass);
        }

        // ---- screen AO ----------------------------------------------------
        // Everything aux drew (meshes, traced tiles, print beads) supplies its own normal.
        // Realtime uses temporal history; StillAa owns accumulation and supplies the sample seed.
        if ssao_ready {
            let cam = state.view.camera(self.pane);
            // scene_states chose cam's ortho/perspective projection; keep its rig crop and
            // this recording's StillAa jitter so the camera describes the exact aux pixels.
            let clip_from_world = Mat4f::mul(&fab_state.projection, &cam.view());
            if let Some(camera) = CortaoCamera::from_clip(&clip_from_world.v) {
                let min_occluder = print_frame.map_or(0.0, |frame| {
                    let preview = Arc::downgrade(&frame.preview);
                    match &self.ssao_floor_for {
                        Some((cached, floor)) if cached.ptr_eq(&preview) => *floor,
                        _ => {
                            let floor = sliced_min_occluder(&frame.preview);
                            self.ssao_floor_for = Some((preview, floor));
                            floor
                        }
                    }
                });
                // In particular, the orthographic decomposition moves the view origin.
                // Aux is in metres already, but its radial depth must be reconstructed before
                // applying this rigid change of frame (normals use its rotation alone).
                let cortao_from_aux = Mat4f::mul(&Mat4f { v: camera.view_from_world }, &fab_state.view.invert());
                let prepass = self.ssao.prepass_draw_mut().unwrap();
                let vars = &mut prepass.draw_super.draw_vars;
                set_texture_named(vars, cx.cx, live_id!(depth_tex), &self.aux_texture);
                vars.set_uniform(cx.cx, live_id!(u_aux_inv_projection), &fab_state.projection.invert().v);
                vars.set_uniform(cx.cx, live_id!(u_cortao_from_aux), &cortao_from_aux.v);
                let table = self.ssao_table.get_or_insert_with(|| CortaoTable::new(cx.cx, CortaoTier::High.rays()));
                let reset = self.ssao_reset || self.ssao_ortho != Some(camera.ortho) || self.ssao_floor != Some(min_occluder);
                if reset {
                    self.ssao_age = 0;
                }
                let input = CortaoInput {
                    size: rect.size,
                    depth: &self.aux_texture,
                    depth_frame: CortaoDepth::Current,
                    camera,
                    tier: CortaoTier::High,
                    config: CortaoConfig {
                        radius: PLATE_AO_RADIUS,
                        min_occluder,
                        ..CortaoConfig::default()
                    },
                    frame: still_sample.map_or(u64::from(self.ssao_age), u64::from),
                    history: still_sample.is_none(),
                    reset,
                    table,
                };
                if let Some(CortaoOutput::Visibility(texture)) = self.ssao.run(cx, &input, lit_id) {
                    self.ssao_texture = Some(texture);
                    self.ssao_reset = false;
                    self.ssao_age = self.ssao_age.wrapping_add(1);
                    self.ssao_ortho = Some(camera.ortho);
                    self.ssao_floor = Some(min_occluder);
                    self.aux_pass.set_pass_parent(cx.cx, self.ssao.first_pass_id().unwrap());
                }
                // The half-size upsample's pipeline is made on its first run: a
                // still view without it waits for DrawShadersReady as above.
                self.ssao_waiting |= self.ssao.pipelines_pending();
            }
        }
        self.ssao_on = self.ssao_texture.is_some();

        // ---- lit ----------------------------------------------------------
        // The depth buffer is INHERITED, not cleared: every fragment the lit
        // pass shades is one the aux pass already proved visible.
        self.lit_pass.set_size(cx, rect.size);
        self.lit_pass.set_color_texture(
            cx,
            &self.lit_texture,
            DrawPassClearColor::ClearWith(self.clear_color),
        );
        self.lit_pass
            .set_depth_texture(cx, &self.depth_texture, DrawPassClearDepth::ClearWith(1.0));
        cx.begin_pass(&self.lit_pass, None);
        self.lit_pass.set_size(cx, rect.size);
        self.lit_pass.set_camera(cx.cx, render_state.view, render_state.projection);
        self.lit_records += 1;
        // The meshes take the AO on their ambient fill through the renderer's own hook (never
        // the sun or the cascades); the composite adds its share of the direct light.
        {
            let mut scene = scene_handle.borrow_mut();
            let scene = &mut *scene;
            Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).set_ssao(self.ssao_texture.as_ref().map(|t| (t.clone(), PLATE_AO_STRENGTH)));
        }
        let pass_size = cx.current_pass_size();
        self.print_list.begin_always(cx);
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        {
            let cx3d = &mut Cx3d::new(cx.cx);
            self.draw_lit_pass(cx3d, render_state, state, world);
            let mut scene = scene_handle.borrow_mut();
            if let Some(frame) = force_frame {
                cx3d.begin_scene_3d(render_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                self.draw_calls += scene.force_layer.draw(cx3d, frame, Some(&light), &mut self.draw_force_aux, &mut self.draw_force);
                if let Some(previous) = previous { let _ = cx3d.set_scene_world_transform_3d(previous); }
                cx3d.end_scene_3d();
            }
            if scene.board_layer.drawable() {
                cx3d.begin_scene_3d(render_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                let scene = &mut *scene;
                let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
                // Under the slice the board is a ghost: shown where it sits, dimmed, never sliced.
                let light = if state.parts_under_slice() { TileLight { exposure: light.exposure * BOARD_GHOST, ..light } } else { light };
                self.draw_calls += scene.board_layer.draw(cx3d, Some((&light, &*renderer)), &mut self.draw_board_aux, &mut self.draw_board, &self.board_moves);
                let ao = self.ssao_texture.as_ref()
                    .filter(|_| self.ssao_on && state.view.shading == Shading::Realtime)
                    .map(|t| (t, PLATE_AO_STRENGTH));
                self.draw_calls += scene.board_layer.draw_bodies(cx3d, Some((&light, &*renderer)), ao, state.floor_shown_in(self.pane), &mut self.draw_body_aux, &mut self.draw_body, &self.board_moves, &state.elec_selected);
                self.body_emitting = scene.board_layer.bodies_emitting_lens() || scene.board_layer.has_screen_glass();
                if let Some(previous) = previous { let _ = cx3d.set_scene_world_transform_3d(previous); }
                cx3d.end_scene_3d();
            }
        }

        {
            let mut scene = scene_handle.borrow_mut();
            let scene = &mut *scene;
            let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
            renderer.bind_output_dither(cx.cx, &mut self.sdf_tile_lit.draw_vars);
            renderer.bind_ibl(cx.cx, &mut self.sdf_tile_lit.draw_vars);
        }
        let scene = scene_handle.borrow();
        let calls = scene.sdf.borrow_mut().draw(cx, self.pane, sdf::SdfPass::TileLit { draw: &mut self.sdf_tile_lit, light: &light, accent: self.color_pick }, &render_state.view, &fab_state.projection, rect.size, render_from_fab(), &planes, state);
        self.draw_calls += calls;
        self.parts_drawn = calls as usize + scene.glass_layer.shown_under(state, poses) + state.plate.parts.iter().filter_map(|p| p.element).filter(|e| !sdf_drawn.contains(e) && state.is_drawn_in(*e, self.pane)).count();
        self.sdf_gpu_prof(cx);
        cx.end_pass_sized_turtle();
        self.print_list.end(cx);
        cx.end_pass(&self.lit_pass);
        drop(scene);
        if self.body_emitting || self.pipe_glow_key.is_some() {
            // Install the halo shaders with the bodies, before Play needs the layer.
            self.bloom.prepare_draws(cx.cx);
        } else if self.bloom.first_pass_id().is_some() {
            // Leave no parent link into the chain's released pass slots.
            self.glow_pass.detach(cx.cx);
            self.bloom = BloomPass::default();
        }
        self.time_tracer_look(cx.cx);
    }

    /// Records the composite pass: the lit image with Solid's look, outlines, hover, x-ray and
    /// section caps, then sorted glass and the grid.
    /// `moving`: the view changes from frame to frame (not a still view's sample), so on a hi-dpi
    /// display See-Through marches half the pixels; a still view marches them all.
    fn record_comp_pass(&mut self, cx: &mut Cx2d, rect: Rect, state: &AppState, fab_state: SceneState3D, light: &TileLight, planes: &[[f32; 4]], print_frame: Option<&print::PrintFrame>, moving: bool) {
        // ---- See-Through at half the pixels, before the composite that reads it ----
        let half = moving && cx.current_dpi_factor() >= SEE_HALF_DPI && self.debug_off as u32 & 1024 == 0 && self.record_see_pass(cx, rect, state, fab_state);
        self.see_half = half;
        if !half {
            // its pass would paint again with every composite, over a picture nothing reads
            self.see_pass.withdraw(cx.cx);
        }
        // ---- composite ----------------------------------------------------
        self.comp_pass.set_size(cx, rect.size);
        self.comp_pass.set_color_texture(
            cx,
            &self.comp_texture,
            DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 1.0)),
        );
        cx.begin_pass(&self.comp_pass, None);
        self.comp_pass.set_size(cx, rect.size);
        self.comp_pass.set_camera(cx.cx, fab_state.view, fab_state.projection);
        // A DRAW LIST, FIRST, ALWAYS. `begin_pass` clears the pass's
        // `main_draw_list_id`; whatever calls `begin_always` first becomes it,
        // and anything drawn before that lands in the *enclosing* list —
        // which belongs to the window pass. That is what painted this
        // viewport's composite quad over the top bar (at pass-local (0,0),
        // i.e. the window origin) and left the real viewport rect showing an
        // empty composite target. Both symptoms, one cause.
        self.comp_list.begin_always(cx);
        let pass_size = cx.current_pass_size();
        cx.begin_root_turtle(pass_size, Layout::flow_overlay());
        self.draw_comp_pass(cx, rect, state, fab_state, light, planes);
        {
            let scene_handle = self.scene().clone();
            let mut scene = scene_handle.borrow_mut();
            let scene = &mut *scene;
            let poses = state.pane_poses(self.pane);
            if scene.glass_layer.shown_under(state, poses) > 0 {
                // This is the display target: glass uses the opaque quad's exact exposure
                // and balance, and samples aux rather than feeding its depth or AO.
                let comp = &self.draw_comp.draw_super.draw_vars;
                for name in [live_id!(u_exposure), live_id!(u_balance)] {
                    if let Some((at, len)) = comp.uniform_range(cx.cx, name) {
                        self.draw_glass.draw_vars.set_uniform(cx.cx, name, &comp.dyn_uniforms[at..at + len]);
                    }
                }
                let (cut, _) = cut_planes(layer_cut(state));
                self.draw_glass.draw_vars.set_uniform(cx.cx, live_id!(clip_cut), &cut);
                let camera = state.view.camera(self.pane);
                scene.glass_layer.order(camera.eye, camera.forward(), state, poses, self.pane);
                let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
                let cx3d = &mut Cx3d::new(cx.cx);
                cx3d.begin_scene_3d(fab_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                self.draw_calls += scene.glass_layer.draw(cx3d, &mut self.draw_glass, &self.aux_texture, light, renderer, planes);
                if let Some(previous) = previous { let _ = cx3d.set_scene_world_transform_3d(previous); }
                cx3d.end_scene_3d();
            }
        }
        let dpi = cx.current_dpi_factor() as f32;
        let px = [rect.size.x as f32 * dpi, rect.size.y as f32 * dpi, rig_mark_dpi(dpi)];
        {
            let cx3d = &mut Cx3d::new(cx.cx);
            self.draw_comp_overlays(cx3d, fab_state, state, px, print_frame);
            if !half {
                self.draw_see_through(cx3d, fab_state, state, px);
            }
        }
        if half {
            self.draw_see_up(cx, rect);
        }
        {
            let cx3d = &mut Cx3d::new(cx.cx);
            self.issue_spot_drawn = None;
            if self.role == PaneRole::Plate && !state.print.monitor_holds() {
                if let Some(spot) = &state.print.issue_spot {
                    let opacity = spot.opacity(Cx::time_now());
                    if opacity > 0.0 {
                        cx3d.begin_scene_3d(fab_state);
                        let vars = &mut self.draw_issue_spot.draw_vars;
                        vars.set_uniform(cx3d.cx, live_id!(u_opacity), &[opacity]);
                        let calls = print::draw_markers(cx3d, &spot.instances, &mut self.print_palette, None, vars);
                        self.draw_calls += calls;
                        if calls > 0 { self.issue_spot_drawn = Some((spot.clone(), opacity)); }
                        cx3d.end_scene_3d();
                    }
                }
            }
        }
        cx.end_pass_sized_turtle();
        self.comp_list.end(cx);
        cx.end_pass(&self.comp_pass);
    }

    fn pick(&self, state: &AppState, rect: Rect, pos: DVec2) -> Option<RayHit> {
        if state.scene.is_empty() {
            return None;
        }
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        pick_ray_in(state, self.pane, &proj.ray(pos))
    }


    /// A click at `pos` in a view that selects parts: the part whose body is under it, unless a
    /// nearer surface of the plate hides it (a closed case hides what it holds; a board never
    /// hides its own parts), is selected in every Electronics view; a click on no part clears the
    /// selection. A live control's press (a switch, a knob) is the control's, not a selection.
    /// Cost: a plate pick and a box test a body, on a click.
    fn select_part_at(&self, cx: &mut Cx, state: &AppState, rect: Rect, pos: DVec2) {
        let ray = ViewProjector::new(*state.view.camera(self.pane), rect).ray(pos);
        let body = self.scene().borrow().board_layer.comp_at(&ray, &self.board_moves);
        let board = |element: ElementId| state.plate.parts.iter().any(|p| p.is_board() && p.element == Some(element));
        let blocker = self.pick(state, rect, pos).filter(|hit| !board(hit.element));
        let comp = body.filter(|(_, t)| blocker.is_none_or(|hit| hit.t >= *t)).map(|(comp, _)| comp);
        use fab_elec_design::{Focus, FocusKind, FocusOrigin};
        cx.action(match comp {
            Some(comp) => fab_elec_view::ViewAction::Focus(Focus { kind: FocusKind::Part, sel: fab_elec_core::Sel::Comp(comp), origin: FocusOrigin::Scene }),
            None => fab_elec_view::ViewAction::Unfocus,
        });
    }

    /// A momentary input under or near `pos` ([`Self::pick_control`]), playing or not.
    fn pick_press(&self, cx: &Cx, state: &AppState, rect: Rect, pos: DVec2) -> Option<fab_elec_core::CompRef> {
        self.live_control(cx, state, rect, pos).and_then(|(comp, knob)| (!knob).then_some(comp))
    }

    /// The live control under or near `pos` and whether it is a knob: [`Self::pick_control`] of
    /// the bench's knobs and momentary inputs.
    fn live_control(&self, cx: &Cx, state: &AppState, rect: Rect, pos: DVec2) -> Option<(fab_elec_core::CompRef, bool)> {
        let sim = &cx.get_global_ref::<fab_elec_view::ElecLive>()?.sim;
        let (knobs, momentary) = (sim.index.knobs(), sim.index.momentary());
        if knobs.is_empty() && momentary.is_empty() { return None; }
        self.pick_control(state, rect, pos, &|comp, knob| if knob { knobs.iter().any(|k| k.0 == comp) } else { press::LivePress::allowed(comp, momentary) })
    }

    /// The live control under or near `pos` of those `takes` allows (a knob a drag turns, else a
    /// button), and whether it is a knob: a plate part bound to one (a printed knob or keycap:
    /// `part_turns`, `part_presses`); else the nearer board shaft or actuator the ray meets, where
    /// no other part is nearer (the device's case and its boards never hide a control); else, when
    /// the pointer rests on no other part, the control whose grip shows nearest it within
    /// [`GRAB_PT`], so a small knob at a far zoom is still easy to take. Cost: one plate pick and
    /// a ray test a control, plus a projection a control when those miss; on pointer moves and
    /// presses only.
    fn pick_control(&self, state: &AppState, rect: Rect, pos: DVec2, takes: &dyn Fn(fab_elec_core::CompRef, bool) -> bool) -> Option<(fab_elec_core::CompRef, bool)> {
        let bound = || state.part_turns.iter().map(|p| (p.element, p.comp, true)).chain(state.part_presses.iter().map(|p| (p.element, p.comp, false)));
        let hit = self.pick(state, rect, pos);
        if let Some((_, comp, knob)) = hit.as_ref().and_then(|hit| bound().find(|b| b.0 == hit.element)) {
            if takes(comp, knob) { return Some((comp, knob)); }
        }
        let shell = |element: ElementId| state.device.case.contains(&element) || bound().any(|b| b.0 == element)
            || state.plate.parts.iter().any(|p| p.is_board() && p.element == Some(element));
        let blocker = hit.filter(|hit| !shell(hit.element));
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        let scene = state.scene_gpu.borrow();
        let ray = proj.ray(pos);
        let exact = [(scene.board_layer.turn_at(&ray, &self.board_moves), true), (scene.board_layer.press_at(&ray, &self.board_moves), false)].into_iter()
            .filter_map(|(at, knob)| at.filter(|(comp, _)| takes(*comp, knob)).map(|(comp, t)| (comp, knob, t)))
            .min_by(|a, b| a.2.total_cmp(&b.2));
        if let Some((comp, knob, distance)) = exact {
            if blocker.is_none_or(|hit| hit.t >= distance) { return Some((comp, knob)); }
        }
        if blocker.is_some() { return None; }
        let mut best: Option<(fab_elec_core::CompRef, bool, f64)> = None;
        for grip in self.control_grips.iter().filter(|g| takes(g.comp, g.knob)) {
            let Some(p) = proj.project(grip.at) else { continue };
            let d = (p - pos).length();
            if d <= GRAB_PT && best.is_none_or(|(_, _, b)| d < b) { best = Some((grip.comp, grip.knob, d)); }
        }
        best.map(|(comp, knob, _)| (comp, knob))
    }

    /// The live controls' grips as this pane shows them, in plate metres: each bound plate part's
    /// centre where the pane poses it, then each board knob shaft's and button actuator's top;
    /// only those the bench has as a knob or a momentary input (a board of another design, while
    /// one loads, has none). Kept a draw for the near grab and the view's description.
    /// O(controls x inputs), and nothing while the bench has none.
    fn collect_grips(&mut self, cx: &Cx, state: &AppState, boards: &board::BoardLayer) {
        self.control_grips.clear();
        let Some(live) = cx.get_global_ref::<fab_elec_view::ElecLive>() else { return };
        let (knobs, momentary) = (live.sim.index.knobs(), live.sim.index.momentary());
        if knobs.is_empty() && momentary.is_empty() { return; }
        let takes = |grip: &ControlGrip| if grip.knob { knobs.iter().any(|k| k.0 == grip.comp) } else { momentary.contains(&grip.comp) };
        let bound = state.part_turns.iter().map(|p| (p.element, p.comp, true))
            .chain(state.part_presses.iter().map(|p| (p.element, p.comp, false)));
        for (element, comp, knob) in bound {
            if !state.is_drawn_in(element, self.pane) { continue; }
            let Some(el) = state.scene.element(element) else { continue };
            let centre = crate::model::bounds::aabb_center(&el.bounds);
            let at = state.pose_in(self.pane, element).map_or(centre, |pose| crate::model::state::pose_apply(&pose, centre));
            self.control_grips.push(ControlGrip { comp, knob, at });
        }
        for knob in [true, false] {
            self.control_grips.extend(boards.port_tips(knob, &self.board_moves).into_iter().map(|(comp, at)| ControlGrip { comp, knob, at }));
        }
        self.control_grips.retain(takes);
    }

    fn pick_screen(&self, cx: &Cx, state: &AppState, rect: Rect, pos: DVec2) -> Option<(fab_elec_core::CompRef, (f32, f32))> {
        if state.view.pane(self.pane).mode != PaneMode::Raster { return None; }
        let sim = &cx.get_global_ref::<ElecLive>()?.sim;
        let scene = state.scene_gpu.borrow();
        if !sim.playing || !scene.board_layer.has_screen_glass() { return None; }
        let ray = ViewProjector::new(*state.view.camera(self.pane), rect).ray(pos);
        let (comp, distance, texel) = scene.board_layer.screen_at(&ray, None, &self.board_moves)?;
        sim.index.touch_target(comp)?;
        if self.pick(state, rect, pos).is_some_and(|hit| hit.t < distance) { return None; }
        Some((comp, texel))
    }

    /// A live knob under or near `pos` ([`Self::pick_control`]) and its place now (turned in any
    /// view, else where the lowering starts it).
    fn pick_turn(&self, cx: &Cx, state: &AppState, rect: Rect, pos: DVec2) -> Option<(fab_elec_core::CompRef, f64)> {
        let (comp, knob) = self.live_control(cx, state, rect, pos)?;
        if !knob { return None; }
        let sim = &cx.get_global_ref::<fab_elec_view::ElecLive>()?.sim;
        let start = sim.index.knobs().iter().find(|k| k.0 == comp)?.1;
        Some((comp, sim.turned.iter().find(|t| t.0 == comp).map_or(start, |t| t.1)))
    }

    /// The readout by a knob a drag turns: its name and place, right of where the drag took it.
    /// None takes it down. Cost: one tip request a sent turn.
    fn show_turn_tip(&mut self, cx: &mut Cx, place: Option<f64>) {
        use makepad_widgets_core::tip::{TipAction, TipPlace, TipRequest};
        let Some((at, name)) = &self.turn_tip else { return };
        match place {
            Some(place) => {
                let mut request = TipRequest::plain(format!("{name}  {place:.2}"), Rect { pos: *at - dvec2(12.0, 12.0), size: dvec2(24.0, 24.0) });
                request.place = TipPlace::Right;
                request.delay_secs = 0.0;
                cx.widget_action(self.uid, TipAction::HoverInWith(request));
            }
            None => {
                self.turn_tip = None;
                cx.widget_action(self.uid, TipAction::HoverOut);
            }
        }
    }

    /// The name of the control under the pointer, by it, while no drag turns one: what a press
    /// takes. One tip request when the control changes.
    fn show_control_tip(&mut self, cx: &mut Cx, control: Option<fab_elec_core::CompRef>, at: DVec2) {
        use makepad_widgets_core::tip::{TipAction, TipPlace, TipRequest};
        if self.live_turn.claimed() || control == self.hover_control { return; }
        self.hover_control = control;
        self.area.redraw(cx);
        match control.and_then(|comp| Some(cx.get_global_ref::<fab_elec_view::ElecLive>()?.knob_name(comp))) {
            Some(name) => {
                let mut request = TipRequest::plain(name, Rect { pos: at - dvec2(12.0, 12.0), size: dvec2(24.0, 24.0) });
                request.place = TipPlace::Right;
                request.delay_secs = 0.0;
                cx.widget_action(self.uid, TipAction::HoverInWith(request));
            }
            None => cx.widget_action(self.uid, TipAction::HoverOut),
        }
    }

    fn held_publication_changed(&self, cx: &Cx) -> bool {
        match (&self.body_held_drawn, cx.get_global_ref::<fab_elec_view::ElecLive>().map(|live| &live.sim.held)) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        }
    }

    fn send_touch(&self, cx: &mut Cx, comp: fab_elec_core::CompRef, down: bool) {
        // A finger off the glass's low edges has no texel; the farthest one is off the glass too,
        // so the host ends that finger as it ends one past the high edges.
        let texel = |v: f32| if v < 0.0 { u16::MAX } else { v.floor().min(f32::from(u16::MAX)) as u16 };
        let (x, y) = self.touch_texel;
        let cmd = fab_elec_bench::SimCmd::Board(fab_elec_bench::BoardInput::Touch {
            comp, x: texel(x), y: texel(y), down,
        });
        cx.action(fab_elec_view::live::sim::SimAction::Cmd(cmd));
    }

    fn release_press(&mut self, cx: &mut Cx) {
        if let Some(comp) = self.live_touch.take() {
            self.send_touch(cx, comp, false);
        } else if let Some(cmd) = self.live_press.up() {
            cx.action(fab_elec_view::live::sim::SimAction::Cmd(cmd));
        } else {
            return;
        }
        self.press_claimed = false;
        self.press_digit = None;
        self.pointer_held = false;
        self.last_pointer = None;
    }


    fn pick_marker(&self, state: &AppState, rect: Rect, pos: DVec2) -> Option<usize> {
        if crate::tools::paint::active(state) || crate::tools::sculpt::active(state) { return None; }
        let preview = state.slice_shown().is_some_and(|p| p.has_beads());
        if preview && state.print.stale { return None; }
        let markers = state.print.markers()?;
        if markers.count() == 0 { return None; }
        let ray = ViewProjector::new(*state.view.camera(self.pane), rect).ray(pos);
        let mut nearest = self.pick(state, rect, pos).map(|h| h.t).unwrap_or(f32::INFINITY);
        let mut picked = None;
        for (i, issue) in state.print.report().issues.iter().enumerate() {
            if preview && issue.layer.is_some_and(|l| l < state.print.play.layer_lo || l > state.print.play.layer_hi) { continue; }
            let [x, y, z] = issue.at_mm.map(|v| v as f32 * 0.001);
            // The two retained cross bars, in metre-space. Picking stays out
            // of the element model and runs only on pointer input.
            for (dx, dy) in [(0.002, 0.0004), (0.0004, 0.002)] {
                let bounds = Aabb { min: vec3(x-dx, y-dy, z-0.0004), max: vec3(x+dx, y+dy, z+0.0004) };
                if let Some(t) = ray.intersect_aabb(&bounds) {
                    if t < nearest { nearest = t; picked = Some(i); }
                }
            }
        }
        picked
    }

    /// The branch of the shown slice's supports under `pos`, by its disc on a shown layer
    /// nearest the eye ([`crate::print::PlateTree::branch_at`]), none behind a part and none
    /// while the supports are hidden: (its tree's place in the newest trees, its lowest disc).
    fn pick_branch(&self, state: &AppState, rect: Rect, pos: DVec2) -> Option<(usize, u32)> {
        if crate::tools::paint::active(state) || crate::tools::sculpt::active(state) { return None; }
        let p = &state.print;
        if p.trees.is_empty() || !p.play.active || !p.play.supports || p.stale || p.monitor_holds() { return None; }
        let preview = p.showing().filter(|v| v.has_beads())?;
        let z_of = |layer: u32| preview.layers.iter().find(|l| l.layer == layer).map(|l| l.z);
        let lo = z_of(p.play.layer_lo).unwrap_or(f32::NEG_INFINITY) - 1e-3;
        let hi = z_of(p.play.layer_hi).unwrap_or(f32::INFINITY) + 1e-3;
        let ray = ViewProjector::new(*state.view.camera(self.pane), rect).ray(pos);
        // A part in front of a branch hides it, as it hides a marker: a click through a gap
        // between trees onto a part's wall picked a branch behind that wall, whose tint no one
        // could see. Only the part's shown layers hide: the beads of the lowest one stand from
        // the print z of the layer under it.
        let floor = p.play.layer_lo.checked_sub(1).and_then(z_of).unwrap_or(f32::NEG_INFINITY) - 1e-3;
        let shown = |at: Vec3f| (floor..=hi).contains(&(at.z * 1000.0));
        let part = pick_ray_where(state, self.pane, &ray, Some(&shown)).map_or(f32::INFINITY, |h| h.t);
        let (mut nearest, mut picked) = (part, None);
        let m = |v: f32| v * 0.001;
        let boxed = |b: &[[f32; 3]; 2]| Aabb { min: vec3(m(b[0][0]), m(b[0][1]), m(b[0][2])), max: vec3(m(b[1][0]), m(b[1][1]), m(b[1][2])) };
        for (i, tree) in p.trees.iter().enumerate() {
            if ray.intersect_aabb(&boxed(&tree.bounds)).is_none_or(|t| t >= nearest) {
                continue;
            }
            // Each disc as a box, in metre-space.
            for (k, d) in tree.discs.iter().enumerate().filter(|(_, d)| (lo..=hi).contains(&d[2])) {
                let bounds = Aabb { min: vec3(m(d[0] - d[4]), m(d[1] - d[4]), m(d[2] - d[3])), max: vec3(m(d[0] + d[4]), m(d[1] + d[4]), m(d[2])) };
                if let Some(t) = ray.intersect_aabb(&bounds).filter(|t| *t < nearest) {
                    nearest = t;
                    picked = Some((i, k as u32));
                }
            }
        }
        let (i, d) = picked?;
        let tree = &p.trees[i];
        Some((i, tree.branch_at(d, tree.discs[d as usize][2])))
    }

    /// The branch at `(tree, lowest disc)` of the newest trees as it is told apart from one list
    /// of trees to the next: its part's object and its tips.
    fn branch_named(state: &AppState, (i, low): (usize, u32)) -> Option<(String, Vec<[f64; 3]>)> {
        let tree = state.print.trees.get(i).filter(|t| (low as usize) < t.discs.len())?;
        let tips = tree.branch_tips(low);
        (!tips.is_empty()).then(|| (tree.object.clone(), tips))
    }

    /// The marked area under `pos` (`instance/solid`: no trees, print settings) in the Sliced
    /// view (`ParamState::areas`): a face of its bodies under the pointer. A print settings box
    /// only while it is picked (it is drawn only then), and never over a part: a click on the
    /// model picks the model.
    fn pick_area(&self, state: &AppState, rect: Rect, pos: DVec2) -> Option<String> {
        if state.render_mode() != RenderMode::Slice || !state.ui.in_mode(PlateMode::Design) || state.slice_blocks_edits() {
            return None;
        }
        let picked = crate::ui::param_panel::picked_volume(state);
        let mut over_part = None;
        let mut takes = |area: &crate::params::MarkedArea| area.kind != crate::params::AreaKind::Settings
            || (picked.as_deref() == Some(area.object.as_str()) && !*over_part.get_or_insert_with(|| self.pick(state, rect, pos).is_some()));
        let proj = ViewProjector::new(*state.view.camera(self.pane), rect);
        let inside = |t: [DVec2; 3]| {
            let side = |a: DVec2, b: DVec2| (b.x - a.x) * (pos.y - a.y) - (b.y - a.y) * (pos.x - a.x);
            let (d0, d1, d2) = (side(t[0], t[1]), side(t[1], t[2]), side(t[2], t[0]));
            (d0 >= 0.0 && d1 >= 0.0 && d2 >= 0.0) || (d0 <= 0.0 && d1 <= 0.0 && d2 <= 0.0)
        };
        state.params.areas.iter().find(|area| takes(area) && area.bodies.iter().any(|body| {
            let pts: Vec<Option<DVec2>> = body.positions.iter().map(|q| proj.project(vec3(q[0] as f32 * 0.001, q[1] as f32 * 0.001, q[2] as f32 * 0.001))).collect();
            body.triangles.iter().any(|t| match (pts[t[0] as usize], pts[t[1] as usize], pts[t[2] as usize]) {
                (Some(a), Some(b), Some(c)) => inside([a, b, c]),
                _ => false,
            })
        })).map(|area| area.object.clone())
    }

    /// The branches the slice's frame lights, in the newest trees: the hovered one (none when it
    /// is the picked one) and the picked one, while the Sliced view shows the plate's current
    /// slice; each as (its tree, its lowest disc).
    fn branch_lit<'a>(&self, state: &'a AppState) -> [Option<(&'a crate::print::PlateTree, u32)>; 2] {
        let p = &state.print;
        let picked = p.picked_branch();
        let hovered = self.hovered_branch.as_ref().and_then(|(object, tips)| p.branch_of(object, tips)).filter(|h| Some(*h) != picked);
        [hovered, picked].map(|b| b.and_then(|(i, low)| Some((p.trees.get(i)?, low))))
    }

    /// Convert a displayed point to the camera's unmirrored pane coordinates once, at entry.
    /// The reflection is its own inverse, so it also places projected points on the display.
    fn pane_point(&self, abs: DVec2, rect: Rect) -> DVec2 {
        if self.draw_bg.mirror_x == 1.0 {
            dvec2(2.0 * rect.pos.x + rect.size.x - abs.x, abs.y)
        } else {
            abs
        }
    }

    /// The views that show this one's camera draw its hover too (the crosshair, the tint): a
    /// redraw of every Plate view, as one view cannot name another's body. Each draws at most
    /// once a frame; a view that shares no camera redraws none.
    fn redraw_other_pane(&self, cx: &mut Cx) {
        if self.shares_camera {
            crate::ui::redraw(cx, &[ids!(viewport)]);
        }
    }

    /// A ring round the live control the pointer rests on or turns, as wide as the target a
    /// press takes it in ([`GRAB_PT`]).
    fn draw_control_ring(&mut self, cx: &mut Cx2d, state: &AppState, rect: Rect) {
        let Some(comp) = self.hover_control else { return };
        let Some(grip) = self.control_grips.iter().find(|g| g.comp == comp) else { return };
        let Some(point) = ViewProjector::new(*state.view.camera(self.pane), rect).project(grip.at) else { return };
        let point = self.pane_point(point, rect);
        if rect.contains(point) {
            self.draw_control_ring.draw_abs(cx, Rect { pos: point - dvec2(GRAB_PT, GRAB_PT), size: dvec2(GRAB_PT * 2.0, GRAB_PT * 2.0) });
        }
    }

    fn draw_crosshair(&mut self, cx: &mut Cx2d, state: &AppState, rect: Rect) {
        // the point under the pointer of another view of this camera
        if state.scene_gpu.borrow().hovered_pane.is_none_or(|pane| !state.view.same_camera(pane, self.pane)) {
            return;
        }
        let Some(hit) = state.view.hover else { return };
        let Some(point) = ViewProjector::new(*state.view.camera(self.pane), rect).project(hit.point) else { return };
        let point = self.pane_point(point, rect);
        if rect.contains(point) {
            self.draw_crosshair.draw_abs(cx, Rect { pos: point - dvec2(12.0, 12.0), size: dvec2(24.0, 24.0) });
        }
    }

    fn deliver(
        &mut self,
        cx: &mut Cx,
        state: &mut AppState,
        kind: ViewportInputKind,
        pick_at: Option<DVec2>,
    ) -> bool {
        let rect = self.area.rect(cx);
        // of the views of one camera, the last one navigated moves it
        if self.shares_camera {
            if matches!(kind, ViewportInputKind::PointerDown { .. } | ViewportInputKind::Scroll { .. } | ViewportInputKind::KeyDown { .. }) {
                state.scene_gpu.borrow_mut().navigation_pane = Some(self.pane);
            } else if matches!(kind, ViewportInputKind::Frame { .. }) && state.scene_gpu.borrow().navigation_pane != Some(self.pane) {
                self.navigator.stop_motion();
            }
        }
        let hit = pick_at.filter(|_| self.role != PaneRole::Plate || !crate::tools::annotate::drawing()).filter(|p| !(crate::tools::paint::active(state) || crate::tools::sculpt::active(state)) || rect.contains(*p)).and_then(|p| self.pick(state, rect, p));
        let input = ViewportInput { rect, kind, hit };
        if self.role != PaneRole::Plate {
            // Tools live on Plate panes. Suppress their priority while this pane's
            // navigator handles input, then restore the UI choices before dispatch continues.
            let (tool, gizmo, marker) = (state.tool, state.ui.gizmo, state.ui.marker);
            state.tool = Tool::Select;
            state.ui.gizmo = GizmoMode::Off;
            state.ui.marker = false;
            let resp = self.navigator.handle(cx, &input, state);
            self.publish_motion(cx);
            (state.tool, state.ui.gizmo, state.ui.marker) = (tool, gizmo, marker);
            if let Some(cursor) = resp.cursor { cx.set_cursor(cursor); }
            if resp.wants_frames || self.navigator.needs_frames() {
                self.next_frame = cx.new_next_frame();
            }
            if resp.redraw {
                self.area.redraw(cx);
                crate::ui::redraw(cx, &[ids!(viewport)]);
            }
            return resp.consumed;
        }
        if matches!(kind, ViewportInputKind::PointerDown { .. } | ViewportInputKind::PointerMove { .. } | ViewportInputKind::PointerUp { .. } | ViewportInputKind::HoverOut) {
            self.pointer_input = Some(if matches!(kind, ViewportInputKind::PointerMove { buttons: 0, .. }) && crate::tools::picks::active(state) {
                ViewportInput { hit: state.view.hover, ..input }
            } else { input });
        }
        let brush_controls = (state.ui.paint_radius, crate::tools::paint::erasing(state), state.ui.sculpt_radius, crate::tools::sculpt::operation(state));
        let mut resp = self.navigator.handle(cx, &input, state);
        self.publish_motion(cx);
        // A claimed press (`press_claimed`) is the navigator's alone: its click is not a tool's.
        let claimed = self.press_claimed && matches!(kind, ViewportInputKind::PointerDown { .. } | ViewportInputKind::PointerMove { .. } | ViewportInputKind::PointerUp { .. });
        if let Some(placed) = (!resp.consumed && !claimed).then(|| self.force_place(cx, state, &input)).flatten() {
            resp.consumed = placed.consumed;
            resp.redraw |= placed.redraw;
        }
        if !resp.consumed && !claimed {
            let t = self.tools.handle(cx, &input, state);
            resp.consumed = t.consumed;
            resp.redraw |= t.redraw;
            resp.wants_frames |= t.wants_frames;
            if t.cursor.is_some() {
                resp.cursor = t.cursor;
            }
        }
        if let Some(cursor) = resp.cursor {
            cx.set_cursor(cursor);
        }
        if resp.wants_frames || self.navigator.needs_frames() {
            self.next_frame = cx.new_next_frame();
        }
        if resp.redraw {
            // Brush cursors and size readouts are in the stage's sibling tool overlay.
            self.area.redraw(cx);
            if crate::tools::annotate::active(state) {
                // the marker's line is drawn by the overlay, in this frame
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
            }
            if crate::tools::paint::active(state) || crate::tools::sculpt::active(state) {
                crate::ui::redraw(cx, &[ids!(tool_overlay)]);
                if brush_controls != (state.ui.paint_radius, crate::tools::paint::erasing(state), state.ui.sculpt_radius, crate::tools::sculpt::operation(state)) {
                    // Brush radius and Alt-mode changes also appear in the tool strips.
                    crate::ui::redraw(cx, &[ids!(model_ui.context.paint_bar), ids!(model_ui.context.sculpt_bar)]);
                }
            }
        }
        resp.consumed
    }

    /// The Force tool: a click on the part adds `ui.force_kind` there, Alt taking the other
    /// reading of face or hole (a drag still orbits: the navigator takes it first); Esc leaves
    /// the tool and keeps the mode. None for any other input.
    fn force_place(&mut self, cx: &mut Cx, state: &AppState, input: &ViewportInput) -> Option<InputResponse> {
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        if !state.force_tool() || state.heatmap_part().is_none() {
            self.force_press = None;
            return None;
        }
        match input.kind {
            // a double-click: on a load, its typed editor (the first click picked it)
            ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, tap_count } if !mods.shift && tap_count >= 2 => {
                self.force_press = None;
                let force_scene = state.force.scene.clone()?;
                let ray = ViewProjector::new(*state.view.camera(self.pane), input.rect).ray(pos);
                if let Some(point) = scene.force_layer.hit(state, self.pane, &force_scene, &ray) {
                    cx.action(ForceAction::Place { point, flip: mods.alt, edit: true });
                }
                Some(InputResponse::consumed())
            }
            ViewportInputKind::PointerDown { button: PointerButton::Primary, pos, mods, .. } if !mods.shift => {
                self.force_press = Some(pos);
                Some(InputResponse { consumed: true, ..Default::default() })
            }
            ViewportInputKind::PointerUp { button: PointerButton::Primary, pos, mods } => {
                let down = self.force_press.take()?;
                if (pos - down).length() < crate::nav::DRAG_THRESHOLD_PT {
                    let force_scene = state.force.scene.clone()?;
                    let ray = ViewProjector::new(*state.view.camera(self.pane), input.rect).ray(pos);
                    if let Some(point) = scene.force_layer.hit(state, self.pane, &force_scene, &ray) {
                        cx.action(ForceAction::Place { point, flip: mods.alt, edit: false });
                    }
                }
                Some(InputResponse::consumed())
            }
            // Esc lets go of the load picked, then leaves the tool
            ViewportInputKind::KeyDown { key: KeyCode::Escape, .. } => {
                match state.force.focus {
                    Some(_) => cx.action(ForceAction::Focus { object: String::new(), index: None }),
                    None => cx.action(crate::ui::stage::StageAction::Gizmo(GizmoMode::Off)),
                }
                Some(InputResponse::consumed())
            }
            _ => None,
        }
    }

    /// The pointer over the studied part, in a floating tip (hover never moves the layout): with the
    /// Force tool what a click would take there (the app reads the surface: `ForceAction::Hover`,
    /// back as `force.hover`) and the crosshair, else the fields under it.
    fn force_hover(&mut self, cx: &mut Cx, state: &AppState, at: Option<DVec2>, alt: bool) {
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        let force_scene = state.force.scene.clone().filter(|_| state.heatmap_part().is_some());
        let ray = at.map(|p| ViewProjector::new(*state.view.camera(self.pane), self.area.rect(cx)).ray(p));
        let tool = state.force_tool();
        let text = match (&force_scene, ray) {
            // on a handle of the picked load: a press takes the handle
            (Some(_), Some(_)) if tool && state.force.grip => {
                if self.force_under.take().is_some_and(|u| u.is_some()) {
                    cx.action(ForceAction::Hover { at: None });
                }
                cx.set_cursor(MouseCursor::Hand);
                None
            }
            (Some(force_scene), Some(ray)) if tool => {
                let under = scene.force_layer.hit(state, self.pane, force_scene, &ray).map(|p| (p, alt));
                if self.force_under != Some(under) {
                    self.force_under = Some(under);
                    cx.action(ForceAction::Hover { at: under });
                }
                cx.set_cursor(if under.is_some() { MouseCursor::Crosshair } else { MouseCursor::Default });
                under.map(|_| state.force.hover.clone()).filter(|t| !t.is_empty())
            }
            (Some(force_scene), Some(ray)) => scene.force_layer.readout(state, self.pane, force_scene, &ray),
            _ => None,
        };
        if tool && at.is_none() && self.force_under.take().is_some_and(|u| u.is_some()) {
            cx.action(ForceAction::Hover { at: None });
        }
        self.show_force_tip(cx, text.zip(at));
    }

    /// Puts up, moves or takes down the study's floating tip.
    fn show_force_tip(&mut self, cx: &mut Cx, tip: Option<(String, DVec2)>) {
        use makepad_widgets_core::tip::{TipAction, TipPlace, TipRequest};
        match tip {
            Some((text, at)) => {
                let mut request = TipRequest::plain(text.clone(), Rect { pos: at - dvec2(4.0, 4.0), size: dvec2(8.0, 8.0) });
                request.place = TipPlace::Right;
                request.wrap_width = 240.0;
                request.delay_secs = 0.0;
                cx.widget_action(self.uid, TipAction::HoverInWith(request));
                self.force_tip = Some(text);
            }
            None if self.force_tip.take().is_some() => cx.widget_action(self.uid, TipAction::HoverOut),
            None => {}
        }
    }

    fn button_of(device: &DigitDevice) -> PointerButton {
        match device.mouse_button() {
            Some(b) if b.is_middle() => PointerButton::Middle,
            Some(b) if b.is_secondary() => PointerButton::Secondary,
            _ => PointerButton::Primary,
        }
    }

    fn buttons_of(device: &DigitDevice) -> u8 {
        match Self::button_of(device) {
            PointerButton::Primary => 1,
            PointerButton::Secondary => 2,
            PointerButton::Middle => 4,
        }
    }
}

impl WidgetNode for FabViewport {
    fn widget_uid(&self) -> WidgetUid {
        self.uid
    }

    fn walk(&mut self, _cx: &mut Cx) -> Walk {
        self.walk
    }

    fn area(&self) -> Area {
        self.area
    }

    fn redraw(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
    }
}

/// The gains that make light of colour `c` white at its own luminance (none for a light
/// without colour).
fn white_balance(c: Vec3f) -> [f32; 3] {
    let l = 0.2126 * c.x + 0.7152 * c.y + 0.0722 * c.z;
    if !(l > 0.0) || c.x <= 0.0 || c.y <= 0.0 || c.z <= 0.0 {
        return [1.0; 3];
    }
    [l / c.x, l / c.y, l / c.z]
}

/// Studio lighting's exposure against the renderer's meter, and its tone curve's contrast: a
/// part's faces part in value as a studio photograph's do. The meter reads the product studio's
/// map, whose overhead scrim lifts its mean; the bias holds a matte print's lit and shaded faces
/// where the earlier studio had them (each within a quarter stop through the curve), below the curve's
/// shoulder, where a lit orange or white face's sides still differ.
const STUDIO_EXPOSURE_BIAS: f32 = 0.30;
/// The studio's exposure times the light its key gives a face turned to it (`display_exposure`).
/// Set with the key at the map's own strength (1.8853), where the traced studio has it: it
/// holds a matte print's faces where the studio showed them while its key ran at three times
/// that, measured over the gallery plate's 40 cameras (the print's lit faces 2.32x brighter at
/// one exposure, so its lit faces stay put and its shaded ones move a fifth of a stop). The
/// earlier anchor, 0.42355832, held the exposure of that brighter key.
const STUDIO_KEY_EXPOSURE: f32 = 0.98253;
/// How much a convex edge lightens in Realtime (`realtime_edge_light`).
const REALTIME_EDGE_LIGHT: f32 = 0.13;
const STUDIO_CONTRAST: f32 = 1.15;

/// The halo's share of the light above display white: the bloom's threshold is display white,
/// so a lens (or a light-pipe tip) below white is drawn whole and sharp, and above it this share
/// of the excess leaves the lens and the chain's levels, weighted to sum to one, spread it round.
/// A legibility share, not veiling glare (a real lens's is a few percent): a lit LED must read
/// at a glance in an overview, and at 12x white even an ideally spread halo needs about 0.15 for
/// that. Fixed on the std chip LED by measurement, not by eye: at 2 mA in an overview the lens and
/// halo differ from the LED off by 20 levels or more over 3.2 lens areas (4.4 when the halo took
/// a share of all the light), and at 20 mA close up the halo adds nothing five lens widths out.
/// An LED breathing through white gains its halo from zero at half of white, with no step.
/// Never above 0.5.
const GLARE_FRACTION: f32 = 0.4;

/// The composite's exposure for `vs` on the HDR lane (`hdr`), or its factor on the legacy lane's
/// display-scaled light. A studio in Realtime on the HDR lane is exposed for its key: the light
/// the key gives a face turned to it sets the exposure (`STUDIO_KEY_EXPOSURE`), so an edit to the
/// map that leaves the key alone (a card, a scrim, a sweep) moves no face. The renderer's meter
/// reads the map's mean, which every such edit moves. Before the studio's key is prepared,
/// elsewhere, and on the legacy lane (whose lanes carry the map's own exposure): the renderer's
/// meter times the display bias, as before.
fn display_exposure(renderer: &Renderer, vs: &ViewportState, lighting: Lighting, hdr: bool) -> f32 {
    let metered = renderer.exposure() * display_tone(vs, lighting).0;
    let studio = lighting.is_studio() && vs.shading == Shading::Realtime;
    let Some(key) = renderer.ibl_sun().filter(|_| studio && hdr) else { return metered };
    let light = makepad_render::sky::luminance(key.radiance * makepad_render::sun::env_sun_scale(&key));
    if !(light.is_finite() && light > 0.0) { return metered }
    STUDIO_KEY_EXPOSURE / light
}

/// The composite's display transform for `vs` (`FabDisplay`): the exposure, the white balance
/// and the tone curve's contrast. A traced pane shows through the same (`TracedPane::set_display`).
fn display_transform(renderer: &Renderer, vs: &ViewportState, lighting: Lighting, hdr: bool) -> (f32, [f32; 3], f32) {
    // A studio is exposed for its key (`display_exposure`). The renderer meters an environment
    // on its mean: a studio's dark stage reads as a dim scene and the key would burn a bright
    // filament's faces into one colour.
    let exposure = display_exposure(renderer, vs, lighting, hdr);
    // A studio is balanced to its key, as a camera is set to the light it shoots under: a white
    // part reads white, its shadows neutral.
    let studio = lighting.is_studio() && vs.shading == Shading::Realtime;
    let balance = renderer.ibl_sun().filter(|_| studio).map_or([1.0; 3], |key| white_balance(key.radiance));
    (exposure, balance, display_tone(vs, lighting).1)
}

/// The composite's bias on the renderer's exposure and its tone curve's contrast for `vs`: a
/// studio's in Realtime, else the renderer's own.
fn display_tone(vs: &ViewportState, lighting: Lighting) -> (f32, f32) {
    if lighting.is_studio() && vs.shading == Shading::Realtime { (STUDIO_EXPOSURE_BIAS, STUDIO_CONTRAST) } else { (1.0, 1.0) }
}


impl Widget for FabViewport {
    fn semantics(&self, cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Canvas;
        node.name = "Model viewport".into();
        // Read-only inspection of the recording on display: its camera in metres (turned over
        // when hung), the parts it drew, whether it has settled, whether the tracer has more to
        // come for the parts it drew (a look, or views of a part's scan, whose finds change the
        // picture once the user rests), the samples in its still view's average so far
        // (`STILL_AA_SAMPLES`: the anti-aliased picture is complete), whether it took the screen
        // AO, whether it drew the floor (not from under the bed), the issue spot it shows, how
        // often it recorded the scene for a change, moved a resin cut in place and started its
        // still average again (and how often for the composite alone), how often a publication
        // asked it to draw (`records.published`), the board's run's simulated time (`sim_t`;
        // null without the electronics host, 0 before the run has stepped), whether that run has
        // settled (`sim_steady`: no publication follows until a command; null without the host),
        // how many frames its navigation asked for after one it took (`nav_frames`: a glide's, a
        // coast's, a moving drag's), the element under the pointer (`hover`), and the trees'
        // branches: whether one is under the pointer and whether one is pressed, and what the
        // slice's frame lights (the hovered branch, the picked one) with how many beads, and where
        // the picked one stands on the screen. Then whether it draws a circuit board, how many of
        // its part bodies are drawn from a model file (`model_bodies`), and the live
        // controls it shows: each one's name, whether it is a knob, where on the screen a press
        // takes it (window points), whether the bench holds it pressed, and a knob's place; and
        // whether a press on a control is the view's (`claimed`, from its down to its release):
        // a test knows the view took the press and its release before the bench answers either.
        if let Some(key) = &self.scene_key {
            let c = &key.camera;
            let spot = self.issue_spot_drawn.as_ref().map_or_else(|| "null".to_string(), |(s, opacity)| {
                let edges: Vec<String> = s.instances.data().chunks_exact(crate::print::PREVIEW_SLOTS).map(|e| {
                    let e = crate::print::Bead::of(e);
                    format!(r#"{{"a":{:?},"b":{:?},"layer":{},"role":{}}}"#, e.a, e.b, e.layer, e.role)
                }).collect();
                format!(r#"{{"opacity":{opacity},"segments":[{}]}}"#, edges.join(","))
            });
            let lit = self.branch_spot.as_ref();
            // settled: nothing changes the picture without input, a scene or a board drawing on
            // its way included (an agent acting on a view still loading acts on the wrong one)
            let drawing = cx.get_global_ref::<fab_elec_view::board::BoardDraws>().is_some_and(fab_elec_view::board::BoardDraws::drawing);
            // at rest: the view's own work done (no glide, bake, grab), a scene on its way aside
            let at_rest = self.still_settled() && !self.rig_vgrab_due && !self.rig_grab.active();
            let settled = at_rest && !self.scene_coming && !drawing;
            let scene = self.scene().borrow();
            let rect = self.area.rect(cx);
            let proj = ViewProjector::new(*c, rect);
            let live = cx.get_global_ref::<fab_elec_view::ElecLive>();
            // a control once, at its first grip (a bound plate part's before its board shaft's)
            let first = |i: usize, grip: &ControlGrip| !self.control_grips[..i].iter().any(|g| g.comp == grip.comp);
            let controls: Vec<String> = live.map_or_else(Vec::new, |live| self.control_grips.iter().enumerate().filter(|(i, g)| first(*i, g)).filter_map(|(_, grip)| {
                let at = self.pane_point(proj.project(grip.at)?, rect);
                let start = live.sim.index.knobs().iter().find(|k| k.0 == grip.comp).map(|k| k.1);
                let place = live.sim.turned.iter().find(|t| t.0 == grip.comp).map(|t| t.1).or(start);
                Some(format!(r#"{{"name":{:?},"knob":{},"at":[{:.1},{:.1}],"held":{},"place":{}}}"#,
                    live.knob_name(grip.comp), grip.knob, at.x, at.y, live.sim.held.contains(&grip.comp),
                    place.filter(|_| grip.knob).map_or_else(|| "null".to_string(), |p| format!("{p:.4}"))))
            }).collect());
            // the parts the recording rings as the Electronics views' selection: each one's
            // reference and where its body's centre shows (window points; null: no body drawn)
            let selected: Vec<String> = live.map_or_else(Vec::new, |live| key.elec_selected.iter().map(|comp| {
                let design = live.snap.live();
                let part = design.unit(comp.board).and_then(|unit| unit.circuit.comps.iter().find(|c| c.id == comp.comp));
                let at = scene.board_layer.comp_centre(*comp, &self.board_moves)
                    .and_then(|p| proj.project(p)).map(|p| self.pane_point(p, rect));
                format!(r#"{{"name":{:?},"at":{}}}"#, part.map_or("?", |c| c.refdes.as_str()),
                    at.map_or_else(|| "null".to_string(), |p| format!("[{:.1},{:.1}]", p.x, p.y)))
            }).collect());
            node.description = format!(
                r#"{{"camera":{{"eye":{:?},"target":{:?},"up":{:?},"hung":{},"ortho":{},"ortho_height":{:?}}},"parts_drawn":{},"settled":{},"at_rest":{},"traced":{},"still_samples":{},"test_cut":{},"screen_ao":{},"floor":{},"issue_spot":{},"records":{{"scene":{},"cut_moved":{},"draws":{},"still":{},"comp":{},"published":{}}},"sim_t":{},"sim_steady":{},"nav_frames":{},"branches":{{"hovered":{},"pressed":{},"lit":{{"hovered":{},"picked":{},"beads":{},"picked_box":{}}}}},"boards_drawn":{},"model_bodies":{},"controls":[{}],"claimed":{},"pointer":{},"hover":{},"selected":[{}]}}"#,
                [c.eye.x, c.eye.y, c.eye.z],
                [c.target.x, c.target.y, c.target.z],
                [c.up.x, c.up.y, c.up.z],
                c.hung,
                c.ortho,
                c.ortho_height,
                self.parts_drawn,
                settled,
                at_rest,
                !scene.sdf.borrow().tracing(),
                self.still_aa.count(),
                self.test_cut.map_or_else(|| "null".to_string(), |(at, size)| format!(r#"{{"at":[{},{},{}],"size":[{},{},{}]}}"#, at[0], at[1], at[2], size[0], size[1], size[2])),
                self.ssao_on,
                self.elem_lut.floor_drawn().unwrap_or(false),
                spot,
                self.scene_records,
                self.cuts_moved,
                self.draws,
                self.still_restarts,
                self.comp_restarts,
                self.published_redraws,
                live.map_or_else(|| "null".to_string(), |live| format!("{:?}", live.sim.status.t)),
                live.map_or_else(|| "null".to_string(), |live| live.sim.status.steady.to_string()),
                self.navigator.frames_asked(),
                self.hovered_branch.is_some(),
                self.pressed_branch.is_some(),
                lit.is_some_and(|s| s.lit[0].is_some()),
                lit.is_some_and(|s| s.lit[1].is_some()),
                lit.map_or(0, |s| s.beads.count()),
                self.picked_box.map_or_else(|| "null".to_string(), |b| format!("{b:?}")),
                scene.board_layer.drawable(),
                scene.board_layer.model_bodies(),
                controls.join(","),
                self.press_claimed,
                self.pointer_seen.map_or_else(|| "null".to_string(), |p| format!("[{:.1},{:.1}]", p.x, p.y)),
                self.hover_seen.map_or_else(|| "null".to_string(), |e| e.0.to_string()),
                selected.join(","),
            );
        }
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get::<AppState>() else { self.release_press(cx); return };
        if self.parked {
            // Discard completed readbacks of any diagnostic view bands still in flight.
            if matches!(event, Event::Signal) { let _ = self.rig_grab.take(cx); }
            return;
        }
        if state.view.panes.get(self.pane.0 as usize).is_none_or(Option::is_none) {
            self.release_press(cx);
            return;
        }
        let link = state.view.pane(self.pane).link;
        self.draw_bg.mirror_x = if link.how() == Some(LinkHow::Through) { 1.0 } else { 0.0 };
        self.shares_camera = link.source().is_some() || state.view.has_followers(self.pane);
        self.adopt_scene(state);
        let scene = self.scene().clone();
        let mut scene = scene.borrow_mut();
        // Pool completion signals the UI; polling never waits or drives extra frames.
        if scene.host == Some(self.pane) {
            scene.poll_star_map(cx, &mut self.force_record, self.area);
            scene.poll_studio(cx, &mut self.force_record, self.area);
        }
        let raster = state.view.pane(self.pane).mode == PaneMode::Raster;
        if scene.board_layer.ready(cx, event) || self.held_publication_changed(cx) || (raster && self.screen_layout_drawn.changed(cx)) {
            self.published_redraws += 1;
            self.area.redraw(cx);
        }
        if scene.product_scene.as_ref().is_some_and(product_scene::ProductSceneCache::ready) {
            self.area.redraw(cx);
        }
        if matches!(event, Event::NextFrame(_) | Event::Signal | Event::Actions(_))
            && self.traced.as_ref().is_some_and(|trace| {
                trace.live_changed(cx, scene.product_scene.as_ref())
            })
        {
            self.redraw(cx);
        }
        drop(scene);
        if self.traced.as_mut().is_some_and(|trace| trace.handle_event(cx, event)) {
            self.area.redraw(cx);
        }
        if raster {
            if self.pane == state.view.lead_plate() { self.handle_scan(cx, event); }
            self.handle_tracer(cx, event);
        }
        if let Event::GpuReset = event {
            if let Some(trace) = &mut self.traced {
                trace.invalidate(self.pane, "GPU reset");
            }
            self.desktop_invalidated_at = cx.repaint_id();
            if state.scene_gpu.borrow().host == Some(self.pane) {
                state.scene_gpu.borrow_mut().renderer.gpu_reset();
            } else {
                self.drop_fork();
            }
            self.ssao_reset = true;
            self.force_record = true;
            self.area.redraw(cx);
        }
        if let Event::ScriptReapply | Event::LiveEdit = event {
            self.ssao = CortaoPass::default();
            self.ssao_reset = true;
            self.glow_pass.detach(cx);
            self.bloom = BloomPass::default();
            self.glow_painted = None;
            self.draw_part_glow = None;
            if let Some(trace) = &mut self.traced {
                trace.invalidate(self.pane, "shaders");
                self.area.redraw(cx);
            }
            self.force_record = true;
        }
        let Some(state) = scope.data.get_mut::<AppState>() else {
            return;
        };
        self.set_pane_mode(cx, state.view.pane(self.pane).mode);
        if matches!(event, Event::MouseUp(_) | Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            self.pointer_held = false;
        }
        if matches!(event, Event::WindowLostFocus(_) | Event::Pause | Event::Background) {
            self.release_press(cx);
            // a key held to orbit never comes up here
            self.navigator.release_keys();
        }

        if let Event::Actions(actions) = event {
            for action in shell_actions(actions) {
                let camera_command = camera_command(action);
                // the person's own camera commands act on the view they last used; the document's
                // (a plate opening, its view put back, a workspace switch) on the active Plate
                let target = if person_camera_command(action) { state.view.camera_pane() } else { state.view.active_plate() };
                if camera_command && self.pane == target { state.scene_gpu.borrow_mut().navigation_pane = Some(self.pane); }
                if camera_jump(action) {
                    self.ssao_reset = true;
                }
                // Every Plate view with a camera of its own (a linked one follows its source's)
                // checks it at its next frame when the plate's view file put it back, or when
                // another plate opens while it keeps the last one's: the active one's opening
                // frames it anyway.
                let restored = matches!(action, ShellAction::RestoreView { camera: true, .. })
                    || matches!(action, ShellAction::FrameOpening(_)) && self.pane != state.view.active_plate();
                if restored && state.view.plate_number(self.pane).is_some() && state.view.camera_root(self.pane) == self.pane {
                    self.navigator.check_camera();
                }
                if self.pane != target && camera_command {
                    // Menu and document camera commands act on one view once: the camera pane.
                    // This pane's own pointer/key navigation goes through deliver.
                    self.navigator.stop_motion();
                    self.area.redraw(cx);
                    continue;
                }
                match action {
                    ShellAction::FrameAll => {
                        let b = state.frame_all_bounds_in(self.pane);
                        self.navigator.frame(cx, state, b, true);
                        self.area.redraw(cx);
                    }
                    ShellAction::FrameOpening(b) => {
                        self.navigator.open(*b);
                        self.area.redraw(cx);
                    }
                    // The camera is the plate's as it was left: nothing of the last plate's glide
                    // or coast goes on.
                    ShellAction::RestoreView { camera: true, .. } => {
                        self.navigator.stop_motion();
                        self.area.redraw(cx);
                    }
                    // Monitor's camera is its own: a glide or a coast doesn't carry over into it or
                    // out of it. The other modes share one, and a glide goes on to its mark.
                    ShellAction::SetWorkspace(w) => {
                        let monitor = w.mode_from(state.ui.plate_mode) == Some(PlateMode::Monitor);
                        if std::mem::replace(&mut self.on_monitor_camera, monitor) != monitor {
                            self.navigator.stop_motion();
                            self.ssao_reset = true;
                        }
                    }
                    ShellAction::OrbitAroundSelection => {
                        self.navigator.orbit_around_selection(state);
                        self.area.redraw(cx);
                    }
                    ShellAction::FrameSelected => {
                        if let Some(b) = state.selection_bounds() {
                            self.navigator.frame(cx, state, b, true);
                            self.area.redraw(cx);
                        }
                    }
                    ShellAction::FrameBox(b) => {
                        self.navigator.frame(cx, state, *b, true);
                        self.area.redraw(cx);
                    }
                    ShellAction::FrameFrom { bounds, look, up } => {
                        self.navigator.frame_from(cx, state, *bounds, *look, *up);
                        self.area.redraw(cx);
                    }
                    ShellAction::PresetView(preset) => {
                        self.navigator.preset(cx, state, *preset, true);
                        self.area.redraw(cx);
                    }
                    ShellAction::ToggleOrtho => {
                        let ortho = !state.view.camera(self.pane).ortho;
                        self.navigator.set_ortho(cx, state, ortho);
                        self.area.redraw(cx);
                    }
                    ShellAction::OrbitBy(dx, dy) => {
                        self.navigator.orbit_by(cx, state, *dx, *dy);
                        self.area.redraw(cx);
                    }
                    // what the view shows changed: one frame (the view draws only on change)
                    ShellAction::Loaded(_)
                    | ShellAction::SceneUpdated { .. }
                    | ShellAction::SetPartLook(_)
                    | ShellAction::SetSlicedDetail(_)
                    | ShellAction::SetTracedDetail(_)
                    | ShellAction::SetSoldered(_)
                    | ShellAction::FieldUnavailable { .. }
                    | ShellAction::SdfLive
                    | ShellAction::SetShading(_)
                    | ShellAction::SetOverlays(_)
                    | ShellAction::SetLighting(_)
                    | ShellAction::ToggleXray
                    | ShellAction::ToggleSeeThrough
                    | ShellAction::ToggleGlass
                    | ShellAction::SetSeeOpacity(_)
                    | ShellAction::SetSeeGlow(_) => {
                        self.area.redraw(cx);
                    }
                    _ => {}
                }
                if self.navigator.needs_frames() {
                    self.next_frame = cx.new_next_frame();
                }
            }
        }

        if let Some(ticket) = self.frame_ticket {
            if let Event::Signal = event {
                if let Some(frame) = cx.try_take_texture_readbacks_for(&[ticket]).into_iter().next() {
                    self.frame_ticket = None;
                    match frame.data {
                        Ok(rgba) => Cx::post_action(ShellAction::ViewportFrame(ViewportFrame { revision: self.frame_revision, width: frame.width as u32, height: frame.height as u32, rgba })),
                        Err(e) => log!("fab vp: the project's picture was not read back: {e:?}"),
                    }
                }
            }
        }
        if self.picture_waits_shader && matches!(event, Event::DrawShadersReady) {
            self.picture_waits_shader = false;
            self.redraw(cx);
        }
        if self.ssao_waiting && matches!(event, Event::DrawShadersReady) {
            self.force_record = true;
            self.redraw(cx);
        }
        if self.pipe_glow_key.is_some() && matches!(event, Event::DrawShadersReady) {
            self.glow_painted = None;
            self.redraw(cx);
        }
        if self.items_wait.is_some() {
            // what the recorded scene waits for answered: record again (a pipeline made or
            // failed, the environment's job ended)
            let answered = match event {
                Event::DrawShadersReady => true,
                Event::Signal => {
                    let mut scene = state.scene_gpu.borrow_mut();
                    let scene = &mut *scene;
                    Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id).environment_needs_draw()
                }
                _ => false,
            };
            if answered {
                self.force_record = true;
                self.redraw(cx);
            }
        }
        if self.items_wait_timer.is_event(event).is_some() {
            self.items_wait_timer = Timer::empty();
            if let Some(since) = self.items_wait {
                let mut scene = state.scene_gpu.borrow_mut();
                let scene = &mut *scene;
                let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
                log!("fab vp: the view waits {:.1} s for the renderer: {}", Cx::time_now() - since, renderer.items_waiting_on(cx).join(", "));
            }
        }
        if let Event::Signal = event {
            // the next band is asked in a draw; the view's description says settled again from
            // the draw after the last
            match self.rig_grab.take(cx) {
                None => {}
                Some(Ok(None)) => self.redraw(cx),
                Some(Ok(Some((w, h, rgba)))) => {
                    rig_write_vgrab(w, h, &rgba);
                    self.redraw(cx);
                }
                Some(Err(e)) => {
                    log!("fab vp vgrab: not read back: {e:?}");
                    self.redraw(cx);
                }
            }
        }
        // The live key checks panel power and brightness. LED and panel publications
        // record only the glow layer, preserving the still average.
        let body_live = ((self.body_emitting || self.pipe_glow_key.is_some()) && matches!(event, Event::NextFrame(_) | Event::Signal | Event::Actions(_)))
            .then(|| BodyLiveKeys::of(cx));
        // Borrow the frame event: NextFrame::is_event clones its token HashSet.
        let frame_event = match event {
            Event::NextFrame(frame) if frame.set.contains(&self.next_frame) => Some(frame),
            _ => None,
        };
        if let Some(ne) = frame_event {
            self.next_frame = NextFrame::default();
            // The rebuilding sweep animates over the retained picture (frames only while it is
            // on); a renderer still settling records again; a still view's average takes its
            // next sample.
            // A drag's own moves and the end of another pane's navigation redraw the view: the still
            // average waits for them without asking frames.
            let still_waits = self.still_aa.next_offset().is_some() && !self.navigator.dragging() && !self.camera_moved_elsewhere();
            if self.traced.as_ref().is_some_and(|trace| trace.schedule.wants_frame(true, trace.stale)) || self.issue_spot_drawn.is_some() || self.rebuild_fx.is_some() || self.force_record || still_waits || self.rig_vgrab_due || std::mem::take(&mut self.picture_waits_frame)
                || (self.pipe_glow_key.is_some() && self.draw_part_glow.is_none()) {
                self.redraw(cx);
            }
            let dt = if self.last_frame_time > 0.0 {
                (ne.time - self.last_frame_time).clamp(0.0, 0.1) as f32
            } else {
                1.0 / 60.0
            };
            self.last_frame_time = ne.time;
            self.deliver(
                cx,
                state,
                ViewportInputKind::Frame { dt, time: ne.time },
                None,
            );
        }

        // The electronics host publishes the run's values before the widgets see the event (the
        // run's signal, or a frame it asked for while more is coming): a light that moved shows
        // from this event, and a playing board whose lights hold asks no frames.
        if let Some(live) = body_live {
            if live != self.body_live_painted {
                self.published_redraws += 1;
                self.redraw(cx);
            }
        }

        if matches!(event, Event::MouseMove(_)) {
            // A blocked hover clears the overlays without picking through the covering layer.
            self.pointer_input = Some(ViewportInput { rect: self.area.rect(cx), kind: ViewportInputKind::HoverOut, hit: None });
        }
        let menu_open = self.role == PaneRole::Plate && state.picks.menu.as_ref().is_some_and(|m| m.pane == self.pane);
        if menu_open && self.pointer_held {
            self.menu_press = true;
            self.context_click = false;
        }
        if menu_open && matches!(event, Event::KeyDown(ke) if ke.key_code == KeyCode::Escape) {
            // The same Escape delivered to the Select tool would also end a command or clear the picks.
            self.dismiss_menu(cx, state);
            return;
        }
        // Grips and controls are hit-tested before the viewport. Its menu locks their hits,
        // lifting its own lock just for this ordinary hit so a stroke keeps capture off-view.
        let hit = if menu_open && cx.sweep_lock_area() == Some(self.area) {
            cx.sweep_unlock(self.area);
            let hit = event.hits(cx, self.area);
            cx.sweep_lock(self.area);
            hit
        } else {
            event.hits(cx, self.area)
        };
        if matches!(hit, Hit::FingerDown(_)) { self.menu_press = menu_open; }
        if menu_open || self.menu_press {
            // Live controls and plate markers can claim a press before deliver() and act on
            // its release afterwards. Give the ring its window points before those paths.
            let kind = match &hit {
                Hit::FingerDown(e) => {
                    self.pointer_held = true;
                    Some(ViewportInputKind::PointerDown { button: Self::button_of(&e.device), pos: e.abs, mods: e.modifiers, tap_count: e.tap_count })
                }
                Hit::FingerMove(e) => Some(ViewportInputKind::PointerMove { pos: e.abs, delta: DVec2::default(), mods: e.modifiers, buttons: Self::buttons_of(&e.device) }),
                Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => Some(ViewportInputKind::PointerMove { pos: e.abs, delta: DVec2::default(), mods: e.modifiers, buttons: 0 }),
                Hit::FingerUp(e) => {
                    self.menu_press = false;
                    self.pointer_held = false;
                    self.last_pointer = None;
                    self.context_click = false;
                    Some(ViewportInputKind::PointerUp { button: Self::button_of(&e.device), pos: e.abs, mods: e.modifiers })
                }
                _ => None,
            };
            if let Some(kind) = kind {
                if menu_open {
                    self.deliver(cx, state, kind, None);
                    if matches!(&hit, Hit::FingerUp(e) if e.cancelled || !e.is_over || e.is_sweep) {
                        self.pointer_input = Some(ViewportInput { rect: self.area.rect(cx), kind: ViewportInputKind::HoverOut, hit: None });
                    }
                }
                return;
            }
        }
        match hit {
            Hit::FingerDown(fe) => {
                if self.live_press.claimed() || self.live_turn.claimed() || self.live_touch.is_some() { return; }
                self.pointer_held = true;
                let rect = self.area.rect(cx);
                let pos = self.pane_point(fe.abs, rect);
                cx.set_key_focus(self.area);
                // the view pressed is the one the camera commands act on
                state.view.pressed_pane = Some(self.pane);
                if self.role == PaneRole::Plate {
                    state.ui.set_plate_key(self.pane, self.area);
                    // the Plate pressed is the one the commands without a pointer act on
                    state.view.pressed_plate = self.pane;
                }
                self.last_pointer = Some(pos);
                let button = Self::button_of(&fe.device);
                if button == PointerButton::Primary {
                    if let Some((comp, at)) = self.pick_turn(cx, state, rect, pos) {
                        self.live_turn.down(comp, at, pos.y);
                        self.press_claimed = true;
                        self.press_digit = Some(fe.digit_id);
                        (self.pressed_marker, self.pressed_branch, self.pressed_area) = (None, None, None);
                        // the trace as it stands stays as the ghost, as a Bench knob's turn keeps it
                        cx.action(fab_elec_view::live::sim::SimAction::BeginGesture);
                        cx.set_cursor(MouseCursor::Hand);
                        let name = cx.get_global_ref::<fab_elec_view::ElecLive>().map_or_else(|| "pot".to_string(), |live| live.knob_name(comp));
                        self.turn_tip = Some((fe.abs, name));
                        self.show_turn_tip(cx, Some(at));
                        return;
                    }
                    if let Some(comp) = self.pick_press(cx, state, rect, pos) {
                        let sim = &cx.get_global::<fab_elec_view::ElecLive>().sim;
                        if let Some(cmd) = self.live_press.down(comp, sim.index.momentary()) {
                            self.press_claimed = true;
                            self.press_digit = Some(fe.digit_id);
                            (self.pressed_marker, self.pressed_branch, self.pressed_area) = (None, None, None);
                            cx.action(fab_elec_view::live::sim::SimAction::Cmd(cmd));
                            // Never deliver a button press to the navigator, including its drag.
                            return;
                        }
                    }
                    if let Some((comp, texel)) = self.pick_screen(cx, state, rect, pos) {
                        self.live_touch = Some(comp);
                        self.touch_texel = texel;
                        self.press_claimed = true;
                        self.press_digit = Some(fe.digit_id);
                        (self.pressed_marker, self.pressed_branch, self.pressed_area) = (None, None, None);
                        self.send_touch(cx, comp, true);
                        return;
                    }
                }
                if self.role == PaneRole::Plate && button == PointerButton::Primary && !crate::tools::areas::active(state) {
                    // A press on a marker, a branch or a marked area is claimed (`press_claimed`).
                    let marker = self.pick_marker(state, rect, pos);
                    let branch = if marker.is_none() { self.pick_branch(state, rect, pos).and_then(|b| Self::branch_named(state, b)) } else { None };
                    let area = if marker.is_none() && branch.is_none() { self.pick_area(state, rect, pos) } else { None };
                    self.press_claimed = marker.is_some() || branch.is_some() || area.is_some();
                    (self.pressed_marker, self.pressed_branch, self.pressed_area) = (marker, branch, area);
                }
                let consumed = self.deliver(
                    cx,
                    state,
                    ViewportInputKind::PointerDown {
                        button,
                        pos,
                        mods: fe.modifiers,
                        tap_count: fe.tap_count,
                    },
                    Some(pos),
                );
                if button == PointerButton::Secondary {
                    self.context_click = !consumed;
                }
            }
            Hit::FingerMove(fe) => {
                self.pointer_seen = Some(fe.abs);
                if self.live_press.claimed() { return; }
                if self.live_turn.claimed() {
                    let pos = self.pane_point(fe.abs, self.area.rect(cx));
                    if let Some(cmd) = self.live_turn.moved(pos.y, fe.modifiers.shift) {
                        if let fab_elec_bench::SimCmd::SetParam { value, .. } = &cmd { self.show_turn_tip(cx, Some(*value)); }
                        cx.action(fab_elec_view::live::sim::SimAction::Cmd(cmd));
                    }
                    cx.set_cursor(MouseCursor::Hand);
                    return;
                }
                let rect = self.area.rect(cx);
                let pos = self.pane_point(fe.abs, rect);
                if let Some(comp) = self.live_touch {
                    if self.press_digit == Some(fe.digit_id) {
                        let ray = ViewProjector::new(*state.view.camera(self.pane), rect).ray(pos);
                        if let Some((_, _, texel)) = state.scene_gpu.borrow().board_layer.screen_at(&ray, Some(comp), &self.board_moves) {
                            self.touch_texel = texel;
                            self.send_touch(cx, comp, true);
                        }
                    }
                    return;
                }
                let pos_start = self.pane_point(fe.abs_start, rect);
                // A claimed press that travels is a drag, as the navigator's turns into one: no click.
                if self.press_claimed && (pos - pos_start).length() >= crate::nav::DRAG_THRESHOLD_PT {
                    (self.pressed_marker, self.pressed_branch, self.pressed_area) = (None, None, None);
                }
                if fe.device.mouse_button().is_some_and(|b| b.is_secondary())
                    && (pos - pos_start).length() >= crate::nav::DRAG_THRESHOLD_PT
                {
                    self.context_click = false;
                }
                let delta = self.last_pointer.map(|p| pos - p).unwrap_or_default();
                self.last_pointer = Some(pos);
                let buttons = Self::buttons_of(&fe.device);
                self.deliver(
                    cx,
                    state,
                    ViewportInputKind::PointerMove {
                        pos,
                        delta,
                        mods: fe.modifiers,
                        buttons,
                    },
                    (crate::tools::paint::active(state) || crate::tools::sculpt::active(state)).then_some(pos),
                );
            }
            Hit::FingerUp(fe) => {
                if self.live_turn.claimed() {
                    // FingerCancel and capture theft arrive here as cancelled FingerUp too
                    if self.press_digit == Some(fe.digit_id) && self.live_turn.up() {
                        self.show_turn_tip(cx, None);
                        self.hover_control = None;
                        self.pointer_held = false;
                        self.press_claimed = false;
                        self.press_digit = None;
                        self.last_pointer = None;
                    }
                    return;
                }
                if self.live_press.claimed() || self.live_touch.is_some() {
                    if self.press_digit == Some(fe.digit_id) {
                        self.release_press(cx);
                    }
                    // FingerCancel and capture theft arrive here as cancelled FingerUp too.
                    return;
                }
                self.pointer_held = false;
                let rect = self.area.rect(cx);
                let pos = self.pane_point(fe.abs, rect);
                let pos_start = self.pane_point(fe.abs_start, rect);
                if fe.cancelled || !fe.is_over || fe.is_sweep {
                    self.pointer_input = Some(ViewportInput { rect: self.area.rect(cx), kind: ViewportInputKind::HoverOut, hit: None });
                }
                // A claimed press's release is the navigator's (it ends the drag, if one began); a
                // click picks what was pressed, when the pointer is on it still. It began no stroke.
                if self.press_claimed {
                    self.last_pointer = None;
                    self.deliver(cx, state, ViewportInputKind::PointerUp { button: Self::button_of(&fe.device), pos, mods: fe.modifiers }, Some(pos));
                    self.press_claimed = false;
                    let click = fe.is_over && !fe.cancelled && !fe.is_sweep && (pos - pos_start).length() < crate::nav::DRAG_THRESHOLD_PT;
                    if let Some(index) = self.pressed_marker.take().filter(|_| click) {
                        if self.pick_marker(state, rect, pos) == Some(index) {
                            cx.action(crate::print::PrintAction::SelectIssue(index));
                        }
                    }
                    if let Some(branch) = self.pressed_branch.take().filter(|_| click) {
                        let at = self.pick_branch(state, rect, pos);
                        if at.and_then(|b| Self::branch_named(state, b)) == Some(branch) {
                            cx.action(crate::print::PrintAction::PickBranch(at));
                        }
                    }
                    if let Some(object) = self.pressed_area.take().filter(|_| click) {
                        if self.pick_area(state, rect, pos).as_ref() == Some(&object) {
                            cx.action(PartAction::PickArea { object });
                        }
                    }
                    if fe.cancelled || !fe.is_over || fe.is_sweep {
                        self.pointer_input = Some(ViewportInput { rect, kind: ViewportInputKind::HoverOut, hit: None });
                    }
                    return;
                }
                if self.role == PaneRole::Plate && fe.cancelled && (crate::tools::paint::active(state) || crate::tools::sculpt::active(state)) {
                    crate::tools::paint::cancel();
                    crate::tools::sculpt::cancel();
                    self.last_pointer = None;
                    // Cancelling a stroke clears its viewport preview and sibling brush overlay.
                    crate::ui::redraw(cx, &[ids!(tab_viewport)]);
                    return;
                }
                // A click off the trees lets the picked branch go.
                if self.role == PaneRole::Plate && state.print.picked_branch.is_some() && Self::button_of(&fe.device) == PointerButton::Primary && fe.is_over && !fe.cancelled && !fe.is_sweep
                    && (pos - pos_start).length() < crate::nav::DRAG_THRESHOLD_PT
                {
                    cx.action(crate::print::PrintAction::PickBranch(None));
                }
                self.last_pointer = None;
                let button = Self::button_of(&fe.device);
                let consumed = self.deliver(
                    cx,
                    state,
                    ViewportInputKind::PointerUp {
                        button,
                        pos,
                        mods: fe.modifiers,
                    },
                    Some(pos),
                );
                if fe.cancelled || !fe.is_over || fe.is_sweep {
                    self.pointer_input = Some(ViewportInput { rect: self.area.rect(cx), kind: ViewportInputKind::HoverOut, hit: None });
                }
                if self.selects_parts && button == PointerButton::Primary && fe.is_over && !fe.cancelled && !fe.is_sweep
                    && (pos - pos_start).length() < crate::nav::DRAG_THRESHOLD_PT
                {
                    self.select_part_at(cx, state, rect, pos);
                }
                if self.role == PaneRole::Plate && button == PointerButton::Secondary && std::mem::take(&mut self.context_click)
                    && !consumed && fe.is_over && !fe.cancelled && !fe.is_sweep
                    && (pos - pos_start).length() < crate::nav::DRAG_THRESHOLD_PT
                {
                    let branch = self.pick_branch(state, self.area.rect(cx), pos);
                    if branch.is_some() {
                        cx.action(crate::print::PrintAction::PickBranch(branch));
                    }
                    let area = if branch.is_none() { self.pick_area(state, self.area.rect(cx), pos) } else { None };
                    if let Some(object) = area.clone() {
                        cx.action(PartAction::PickArea { object });
                    }
                    let hit = self.pick(state, rect, pos).and_then(|h| Some((state.plate.part_of(h.element)?, h.point)));
                    let part = hit.as_ref().map(|(i, _)| state.plate.parts[*i].object.clone());
                    let feature = hit.as_ref().and_then(|(i, p)| {
                        let found = &state.plate.parts[*i];
                        let name = crate::tools::paint::feature_at(found, [p.x as f64 * 1000.0, p.y as f64 * 1000.0, p.z as f64 * 1000.0]).filter(|n| !n.is_empty())?;
                        Some(format!("{}/{name}", found.object.split('/').next().unwrap_or(&found.object)))
                    });
                    cx.widget_action(self.uid, ViewportContextMenu { at: pos, branch, area, part, feature });
                }
            }
            Hit::FingerHoverIn(fh) | Hit::FingerHoverOver(fh) => {
                state.view.pointer_pane = Some(self.pane);
                let rect = self.area.rect(cx);
                let pos = self.pane_point(fh.abs, rect);
                let delta = self.last_pointer.map(|p| pos - p).unwrap_or_default();
                self.last_pointer = Some(pos);
                let marker = (self.role == PaneRole::Plate).then(|| self.pick_marker(state, rect, pos)).flatten();
                let marker_changed = marker != self.hovered_marker;
                self.hovered_marker = marker;
                let branch = if self.role == PaneRole::Plate && marker.is_none() { self.pick_branch(state, rect, pos) } else { None };
                let branch = branch.and_then(|b| Self::branch_named(state, b));
                let branch_changed = branch != self.hovered_branch;
                let on_branch = branch.is_some();
                self.hovered_branch = branch;
                let hit = if marker.is_none() && !on_branch { self.pick(state, rect, pos) } else { None };
                let changed = marker_changed || branch_changed ||
                    hit.map(|h| h.element) != state.view.hover.map(|h| h.element);
                let pane_changed = state.scene_gpu.borrow().hovered_pane != Some(self.pane);
                let crosshair_changed = self.shares_camera &&
                    (pane_changed || hit.map(|h| h.point) != state.view.hover.map(|h| h.point));
                state.view.hover = hit;
                self.hover_seen = hit.map(|h| h.element);
                self.pointer_seen = Some(fh.abs);
                let tints = !self.selects_parts;
                {
                    let mut scene = state.scene_gpu.borrow_mut();
                    scene.hovered_pane = Some(self.pane);
                    scene.hover_tints = tints;
                }
                self.deliver(
                    cx,
                    state,
                    ViewportInputKind::PointerMove {
                        pos,
                        delta,
                        mods: fh.modifiers,
                        buttons: 0,
                    },
                    (crate::tools::paint::active(state) || crate::tools::sculpt::active(state)).then_some(pos),
                );
                // the control a press would take: a hand and its name by it before the press
                let control = if marker.is_some() || on_branch { None } else {
                    self.live_control(cx, state, rect, pos).map(|(comp, _)| comp)
                };
                self.show_control_tip(cx, control, fh.abs);
                if marker.is_some() || on_branch || control.is_some() || self.pick_screen(cx, state, rect, pos).is_some() {
                    cx.set_cursor(MouseCursor::Hand);
                }
                if self.role == PaneRole::Plate && (state.force_shown() || self.force_tip.is_some()) {
                    self.force_hover(cx, state, Some(pos), fh.modifiers.alt);
                }
                // A view that tints nothing redraws for its marker or branch only.
                if changed && (tints || marker_changed || branch_changed) {
                    self.area.redraw(cx);
                }
                if changed || crosshair_changed {
                    // The point is window-pass state, not part of the composite recording key.
                    self.redraw_other_pane(cx);
                }
                if crosshair_changed && pane_changed {
                    // This pane now owns the pointer: erase its previous crosshair.
                    self.area.redraw(cx);
                }
            }
            Hit::FingerHoverOut(fh) => {
                if state.view.pointer_pane == Some(self.pane) {
                    state.view.pointer_pane = None;
                }
                self.last_pointer = None;
                self.hover_seen = None;
                self.show_control_tip(cx, None, fh.abs);
                if self.force_tip.is_some() || self.force_under.is_some() {
                    self.force_hover(cx, state, None, false);
                }
                // Marker hover is drawn only in this viewport.
                if self.hovered_marker.take().is_some() | self.hovered_branch.take().is_some() { self.area.redraw(cx); }
                let owns_hover = state.scene_gpu.borrow().hovered_pane == Some(self.pane);
                if owns_hover { state.scene_gpu.borrow_mut().hovered_pane = None; }
                if owns_hover && state.view.hover.is_some() {
                    state.view.hover = None;
                    self.area.redraw(cx);
                    self.redraw_other_pane(cx);
                }
                self.deliver(cx, state, ViewportInputKind::HoverOut, None);
            }
            Hit::FingerScroll(fs) => {
                let rect = self.area.rect(cx);
                let pos = self.pane_point(fs.abs, rect);
                // A control in front already took the wheel.
                if event.scroll_handled(Vec2Index::X) || event.scroll_handled(Vec2Index::Y) {
                    return;
                }
                self.deliver(
                    cx,
                    state,
                    ViewportInputKind::Scroll {
                        delta: fs.scroll,
                        pos,
                        mods: fs.modifiers,
                    },
                    Some(pos),
                );
            }
            Hit::KeyDown(ke) => {
                self.deliver(
                    cx,
                    state,
                    ViewportInputKind::KeyDown {
                        key: ke.key_code,
                        mods: ke.modifiers,
                        repeat: ke.is_repeat,
                    },
                    None,
                );
            }
            // the key focus gone, a held orbit key's release goes elsewhere
            Hit::KeyFocusLost(_) => self.navigator.release_keys(),
            Hit::KeyUp(ke) => {
                self.deliver(
                    cx,
                    state,
                    ViewportInputKind::KeyUp {
                        key: ke.key_code,
                        mods: ke.modifiers,
                    },
                    None,
                );
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get_mut::<AppState>() else { return DrawStep::done() };
        self.edit_moving = state.elec_previewing;
        self.scene_coming = state.scene_coming;
        if state.view.panes.get(self.pane.0 as usize).is_none_or(Option::is_none) {
            return DrawStep::done();
        }
        self.adopt_scene(state);
        let scene_handle = state.scene_gpu.clone();
        let old_area = self.area;
        let debug = (self.debug_view, self.debug_off);
        if self.debug_applied != Some(debug) {
            let changed = self.debug_applied.is_some() || debug != (0.0, 0.0);
            self.debug_applied = Some(debug);
            if changed {
                self.force_record = true;
                self.next_frame = cx.new_next_frame();
                log!("fab vp debug: view={} off={}", self.debug_view, self.debug_off);
            }
        }
        let rect = cx.walk_turtle_with_area(&mut self.area, walk);
        if let Some(state) = scope.data.get_mut::<AppState>() {
            state.ui.refresh_plate_keys(old_area, self.area);
        }
        if rect.size.x <= 1.0 || rect.size.y <= 1.0 {
            return DrawStep::done();
        }
        let Some(state) = scope.data.get_mut::<AppState>() else {
            return DrawStep::done();
        };

        self.parked = false;
        self.navigator.prepare_frame(state, rect);
        let pane = state.view.pane_mut(self.pane);
        pane.parked = false;
        pane.interacting = pane.pointer_held || self.trace_interacting();
        let mut scene = scene_handle.borrow_mut();
        let is_traced = state.view.pane(self.pane).mode == PaneMode::Traced;
        let lighting = state.view.lighting_for(&state.ui);
        if scene.take_host(self.pane) {
            self.drop_fork();
            // Configure the host before a second camera can fork it, including a Traced first draw.
            let casters = scene.casters.clone();
            scene.renderer.set_cascade_casters(Some(casters));
            scene.renderer.set_gpu_lightmap_mode(GpuLightmapMode::Realtime);
            scene.renderer.set_shadow_budget(DEFAULT_SHADOW_BUDGET);
            scene.renderer.set_hdr_output(cx.gpu_info().float16_blend_targets);
            scene.renderer.set_display_transform(true);
            self.force_record = true;
            self.ssao_reset = true;
        }
        if scene.host == Some(self.pane) {
            scene.start_studio(cx.cx, lighting);
        }
        let sdf_sync = if is_traced { None } else {
            // Any raster view can request the shared sky asset. The host installs it on completion.
            scene.start_star_map(cx.cx, lighting);
            Some(scene.sync_fields(cx.cx, state))
        };
        // SDF publication is shared with the eyes, so keep it above this gate. Everything
        // below records only the desktop camera: meshes, slice, shadows and composite.
        // A mirror repaint samples the retained picture without attaching those passes.
        if state.view.pane(self.pane).mode == PaneMode::Raster
            && self.initialized && makepad_xr_stream::XrDesktopFreeze::active(cx)
            && cx.passes[self.comp_pass.draw_pass_id()].painted_serial > self.desktop_invalidated_at
        {
            makepad_xr_stream::XrDesktopFreeze::retain_pass(cx, &self.comp_pass);
            self.collect_grips(cx.cx, state, &scene.board_layer);
            self.force_record = true;
            self.draw_bg.set_scene_texture(&self.comp_texture);
            self.draw_bg.mirror_x = if state.view.pane(self.pane).link.how() == Some(LinkHow::Through) { 1.0 } else { 0.0 };
            self.draw_bg.draw_abs(cx, rect);
            drop(scene);
            self.draw_crosshair(cx, state, rect);
            self.draw_control_ring(cx, state, rect);
        self.draw_control_ring(cx, state, rect);
            return DrawStep::done();
        }
        let SceneSync { board_changed, sdf_record } = if is_traced {
            // The board's placement also defines the linked cameras' mirror plane.
            // Traced panes do not request raster uploads, fields, paint or sky textures.
            SceneSync { board_changed: scene.board_layer.sync(cx.cx, state), sdf_record: false }
        } else {
            scene.sync(cx.cx, state)
        };
        let poses = state.pane_poses(self.pane);
        scene.board_layer.moves_under(state, poses, &mut self.board_moves);
        self.collect_grips(cx.cx, state, &scene.board_layer);
        if !state.part_glows.is_empty() || scene.part_glow.active() {
            scene.part_glow.sync(state);
        }
        let pipe_glow_key = if is_traced || !scene.part_glow.seated_under(state, poses) {
            None
        } else {
            scene.part_glow.key()
        };
        if self.pipe_glow_key != pipe_glow_key {
            if pipe_glow_key.is_some() {
                // A binding can arrive without a new aux/lit recording.
                self.bloom.prepare_draws(cx.cx);
            } else if !self.body_emitting {
                self.glow_pass.detach(cx.cx);
                self.bloom = BloomPass::default();
            }
            self.pipe_glow_key = pipe_glow_key;
        }
        if self.pipe_glow_key.is_some() && self.draw_part_glow.is_none() {
            self.draw_part_glow = cx.cx.cx.try_with_vm(part_glow::DrawFabPartGlow::script_new_with_default);
            // A busy VM retries on a draw, even if the simulation is paused.
            if self.draw_part_glow.is_none() { self.next_frame = cx.new_next_frame(); }
            else { self.glow_painted = None; }
        } else if self.pipe_glow_key.is_none() {
            self.draw_part_glow = None;
        }
        // Every Raster pane rasters owed board art each draw (its raster retires its own recordings).
        if self.held_publication_changed(cx.cx) {
            self.body_held_drawn = cx.get_global_ref::<fab_elec_view::ElecLive>().map(|live| live.sim.held.clone());
            self.force_record |= !is_traced;
        }
        if !is_traced && self.screen_layout_drawn.update(cx.cx) { self.force_record = true; }
        let board_art = !is_traced && scene.board_layer.raster_art(cx, &mut self.board_raster);
        if scene.host != Some(self.pane) && self.review_generation != Some(scene.uploaded_generation) {
            self.review = None;
            self.review_generation = Some(scene.uploaded_generation);
            self.review_environment = None;
            self.force_record = true;
        }
        let mut bounds = state.scene.bounds;
        if let Some(board) = scene.board_layer.bounds(&self.board_moves) {
            bounds = aabb_union(&bounds, &Aabb { min: board.min * 0.001, max: board.max * 0.001 });
        }
        state.view.camera_mut(self.pane).fit_clip_planes(&bounds, (rect.size.x / rect.size.y.max(1.0)) as f32);
        if self.cameras_frame != Some(cx.drawing_id()) && state.view.has_followers(self.pane) {
            // A board's own mid-plane wins; a scene without boards reflects through its
            // bounds' middle z (all in Z-up metres). An empty scene uses the bed plane.
            state.view.mirror_plane = scene.board_layer.mirror_plane(&self.board_moves).unwrap_or_else(|| {
                let z = if aabb_is_empty(&state.scene.bounds) { 0.0 } else { aabb_center(&state.scene.bounds).z };
                [0.0, 0.0, 1.0, -z]
            });
            state.view.settle_cameras();
            self.cameras_frame = Some(cx.drawing_id());
        }
        if state.view.panes.iter().flatten().any(|pane| !pane.parked && pane.mode == PaneMode::Traced) {
            // One plain borrow of the scene, so the poses and the product scene are its disjoint fields.
            let scene = &mut *scene;
            let shared_poses = state.shared_poses();
            let moves = if poses == shared_poses {
                &self.board_moves
            } else {
                scene.board_layer.moves_under(state, shared_poses, &mut self.shared_board_moves);
                &self.shared_board_moves
            };
            let board_poses = scene.board_layer.board_poses(moves, &mut self.traced_board_poses);
            let white_to_trace = if self.hdr { 1.0 / display_exposure(&scene.renderer, &state.view, lighting, true) } else { 1.0 };
            scene.product_scene.get_or_insert_with(Default::default).sync(cx.cx, state, board_poses, white_to_trace, &scene.part_glow);
        } else {
            scene.product_scene = None;
        }
        if is_traced {
            if self.traced.is_none() {
                self.traced = cx.cx.cx.try_with_vm(product_scene::TracedPane::new);
                // Withdraw the previous raster recording when this pane changes mode.
                self.comp_pass.detach(cx.cx);
                self.lit_pass.detach(cx.cx);
                self.aux_pass.detach(cx.cx);
                self.glow_pass.detach(cx.cx);
                self.glow_painted = None;
                self.still_aa = StillAa::default();
                self.body_emitting = false;
                cx.stop_timer(self.items_wait_timer);
                cx.stop_timer(self.sdf_timer);
                cx.stop_timer(self.scan_timer);
                self.items_wait_timer = Timer::empty();
                self.sdf_timer = Timer::empty();
                self.scan_timer = Timer::empty();
                self.items_wait = None;
                scene.sdf.borrow_mut().drop_pane(self.pane);
                self.force_record = false;
            }
            if let Some(trace) = &mut self.traced {
                // Traced light is linear: it takes the HDR lane's display transform.
                let (exposure, balance, contrast) = display_transform(&scene.renderer, &state.view, lighting, true);
                trace.set_display(exposure, balance, contrast);
                let product = scene.product_scene.as_ref().unwrap();
                // Studio lighting traces under the baked studio map, hung as the realtime studio hangs it.
                let environment = scene.studio_light(lighting)
                    .map(|light| (light, stage::studio_trace_frame(state.view.camera(self.pane), scene.studio_keys(lighting).and_then(|keys| keys.sun))));
                if trace.draw(cx, state, self.pane, rect, product, environment) {
                    self.next_frame = cx.new_next_frame();
                }
                self.draw_bg.scene_ready = if let Some(texture) = trace.texture() {
                    self.draw_bg.set_scene_texture(texture);
                    1.0
                } else { 0.0 };
            } else {
                self.draw_bg.scene_ready = 0.0;
                // The VM is busy this draw, so creation has not run yet.
                self.next_frame = cx.new_next_frame();
            }
            self.draw_bg.mirror_x = if state.view.pane(self.pane).link.how() == Some(LinkHow::Through) { 1.0 } else { 0.0 };
            self.draw_bg.draw_abs(cx, rect);
            if self.navigator.needs_frames() { self.next_frame = cx.new_next_frame(); }
            drop(scene);
            self.draw_crosshair(cx, state, rect);
            self.draw_control_ring(cx, state, rect);
        self.draw_control_ring(cx, state, rect);
            return DrawStep::done();
        }
        drop(scene);
        let sdf_sync = sdf_sync.expect("Raster panes synchronize their fields");
        self.set_pane_mode(cx.cx, PaneMode::Raster);
        self.ensure_initialized(cx.cx);
        let mut scene = scene_handle.borrow_mut();
        // Each pane's table hides the floor from its own camera under the bed.
        let lut = self.elem_lut.sync(cx.cx, state, self.pane, &scene.sdf_drawn, scene.sdf_revision, &mut scene.sdf.borrow_mut().paint);
        let lut_changed = lut != elements::LutSync::Kept;
        self.sync_copy_tint(cx.cx, crate::tools::fit::ghost(state, self.pane), lut_changed);
        if scene.host != Some(self.pane) {
            let environment = (scene.renderer.ibl_texture().map(Texture::texture_id), scene.lighting_revision);
            if self.review_environment != Some(environment) {
                self.force_record = true;
            }
        }
        let sdf_drawn = std::mem::take(&mut scene.sdf_drawn);
        self.draw_calls = 0;
        self.debug_frames = self.debug_frames.wrapping_add(1);
        if self.debug_frames < 4 {
            log!(
                "fab vp frame{} rect={:?} dpi={} batches={} model={} lut={:?} passes(aux={:?},lit={:?},comp={:?})",
                self.debug_frames, rect, cx.current_dpi_factor(),
                scene.batch_geometries.len(), scene.model_id, self.elem_lut.size(),
                self.aux_pass.draw_pass_id(), self.lit_pass.draw_pass_id(),
                self.comp_pass.draw_pass_id()
            );
            log!(
                "fab vp textures aux={:?} lit={:?} comp={:?} depth={:?}",
                self.aux_texture.texture_id(),
                self.lit_texture.texture_id(),
                self.comp_texture.texture_id(),
                self.depth_texture.texture_id()
            );
        }

        if board_changed || board_art {
            self.force_record = true;
            self.next_frame = cx.new_next_frame();
        }
        let camera = *state.view.camera(self.pane);
        let (fab_state, render_state) = self.scene_states(&camera, rect, cx.time());

        // The scene passes (aux, lit) are recorded only when what they show changed: the
        // scene, its per-element state, the camera, the light, the print preview. Anything else
        // that redraws the window (a status chip, a spinner, a hover in a panel, the rebuilding
        // sweep below) reuses their retained pictures and costs the GPU nothing here. The
        // composite records with them, or alone when only the hovered part or the resin layer
        // changed.
        let planes = view_planes(state);
        self.test_cut = state.test_cut_shown().then(|| crate::ui::test_prints::shown(state)).flatten().map(|cube| (cube.center, cube.size));
        // A resin layer's cut: the scene passes record with it, and a new layer moves it in
        // place (`move_cut`), so scrubbing the layers records only the composite.
        let cut = layer_cut(state);
        let mut print_frame = print::PrintFrame::of(state, self.print_frame.take());
        // A print frame draws no field parts: their tracer stays unread while it shows, and its
        // finished work asks nothing of this view. Otherwise the scene records for the tracer and
        // the paint's bakes only when they woke the view, a look the tracer owes fell due, or a
        // part's shape is new.
        let sdf_look = (sdf_sync.record || sdf_record) && print_frame.is_none();
        scene.sdf_busy = sdf_sync.busy && print_frame.is_none();
        if let Some(frame) = &mut print_frame { frame.hovered_marker = self.hovered_marker; }
        let lit = self.branch_lit(state);
        BranchSpot::follow(&mut self.branch_spot, state.slice_shown(), lit);
        self.picked_box = self.branch_spot.as_ref().and_then(|s| s.lit[1].as_deref()).and_then(|discs| screen_box(&ViewProjector::new(camera, rect), discs));
        if let Some(frame) = &mut print_frame { frame.lit = self.branch_spot.as_ref().map(|s| s.beads.clone()); }
        // what a click of the Force tool would take, as the app read it after the pointer moved
        if let (Some(at), Some(Some(_)), true) = (self.last_pointer, self.force_under, state.force_tool()) {
            if self.force_tip.as_deref() != Some(state.force.hover.as_str()) {
                let text = (!state.force.hover.is_empty()).then(|| state.force.hover.clone());
                self.show_force_tip(cx.cx, text.map(|t| (t, at)));
            }
        }
        let force_frame = force::ForceFrame::of(state);
        self.head_moves = state.print.follow.head_moves && state.print.monitor_preview().is_some();
        let scene_key = SceneKey::of(state, self.pane, rect.size, cx.current_dpi_factor(), self.hovered_marker, self.clear_color, scene.sdf_revision, scene.see.revision(), planes, cut.is_some(), print_frame.as_ref(), force_frame.as_ref(), self.spare_scene_key.take());
        let planes = planes.as_slice();
        // the tracer's tiles or a look it owes, or a changed element lookup (the
        // meshes the SDF parts hide): record again.
        // (a cut that moves over passes holding a still view's jittered sample records them again)
        let scene_changed = self.force_record || sdf_look || lut_changed || self.scene_key.as_ref() != Some(&scene_key) || (self.scene_jittered && self.cut_drawn != cut);
        let comp_key = (hover_tint(state, &scene), state.resin_shown().and_then(|r| r.image.as_ref()).map(|im| im.id));
        let spot = state.print.issue_spot.as_ref().filter(|s| self.role == PaneRole::Plate && !state.print.monitor_holds() && s.opacity(Cx::time_now()) > 0.0);
        let spot_changed = spot.is_some() || self.issue_spot_drawn.is_some();
        let comp_changed = self.comp_key != Some(comp_key) || spot_changed;
        // Attached every draw, recorded or not: a pass whose attaching list is recorded again
        // without it is orphaned, and so is the chain under it. A draw that adds a sample to
        // the still view's average paints the composite under the blend that reads it.
        drop(scene);
        let FramePlan { record_scene, record_comp, restart_still, wants_sample } = FramePlan::of(
            scene_changed, comp_changed, self.still_settled(), self.still_aa.next_offset().is_some(),
        );
        let sample = if wants_sample { self.still_aa.attach(cx) } else { None };
        if wants_sample && sample.is_none() {
            // its blend is not built or compiled yet: no sample this draw, the next frame asks again
            self.next_frame = cx.new_next_frame();
        }
        match sample {
            // under the blend, attached by it (the window's list attaches the blend)
            Some(aa) => cx.cx.attach_child_pass(self.comp_pass.draw_pass_id(), aa, None),
            None => cx.make_child_pass(&self.comp_pass),
        }
        if restart_still {
            // the still view's average starts again from the next frame that changes nothing
            self.still_aa.reset();
            self.still_restarts += 1;
            self.comp_restarts += u64::from(!record_scene);
            if !self.head_moves {
                self.next_frame = cx.new_next_frame();
            }
        }
        let mut scene = scene_handle.borrow_mut();
        if scene.host != Some(self.pane) && (record_comp || sample.is_some()) {
            let scene = &mut *scene;
            Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
            let review = self.review.as_deref_mut().unwrap();
            // The live-light second-camera handoff, before any pass reads its exposure or IBL.
            review.set_hdr_output(scene.renderer.hdr_output());
            review.set_lod_bias(scene.renderer.lod_bias());
            review.mirror_stream_from(&scene.renderer);
            let environment = (scene.renderer.ibl_texture().map(Texture::texture_id), scene.lighting_revision);
            if self.review_environment != Some(environment) {
                // The mirror also copies the registered-map table. Do that on a new
                // environment only; rotation/intensity already share the texture's meta row.
                review.mirror_environment_from(&scene.renderer);
                self.review_environment = Some(environment);
            }
            // Static instances came with the fork; lights are resolved from this pane's
            // world by draw_lit_pass. Copying a whole review frame allocates each redraw.
            review.mirror_frame_effects_from(&scene.renderer);
            // The fork's cascades have their own fit and borrow the shared field and electronics casters.
            review.set_cascade_casters(Some(scene.casters.clone()));
        }
        let world = (record_comp || sample.is_some()).then(|| stage::stage_world(state, &camera, scene.studio_keys(lighting), scene.studio_map_key()));
        let light = world.as_ref().map(|world| {
            if scene.host == Some(self.pane) {
                scene.sdf_light(world, self.hdr)
            } else {
                let scene = &mut *scene;
                tile_light(Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id), world, self.hdr)
            }
        });
        drop(scene);
        if record_scene {
            // The AO's history carries over a camera move alone: any other change to what the
            // scene passes draw starts it afresh.
            if self.force_record || sdf_look || lut_changed || self.cut_drawn != cut || !self.scene_key.as_ref().is_some_and(|k| k.same_but_camera(&scene_key)) {
                self.ssao_reset = true;
            }
            if self.stats_log {
                let key_changed = self.scene_key.as_ref() != Some(&scene_key);
                let field = if !key_changed { "" } else { self.scene_key.as_ref().map_or("first", |k| k.first_difference(&scene_key)) };
                log!("fab vp record: pane {} force {} sdf {} lut {lut:?} key {field}", self.pane.0, self.force_record, sdf_look);
            }
            self.force_record = false;
            self.spare_scene_key = self.scene_key.replace(scene_key);
            self.record_scene_passes(cx, rect, state, fab_state, render_state, world.as_ref().unwrap(), light.unwrap(), &sdf_drawn, planes, cut, print_frame.as_ref(), force_frame.as_ref(), None);
            self.scene_jittered = false;
            self.scene_records += 1;
            self.stats[1] += 1;
            // The renderer picks its lanes at record time from what has compiled and prepared
            // (a PBR variant, an environment adopted on the next draw): until it is all there,
            // record again when what it waits for answers (handle_event), not every frame.
            let mut scene = scene_handle.borrow_mut();
            let scene = &mut *scene;
            let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
            if renderer.items_ready(cx.cx) {
                self.items_wait = None;
            } else {
                if self.items_wait.is_none() {
                    self.items_wait = Some(Cx::time_now());
                    self.items_wait_timer = cx.start_timeout(ITEMS_WAIT_SAY);
                }
                // a finished preparation to take, or the next one to submit: one more draw
                if renderer.environment_needs_draw() {
                    self.force_record = true;
                    self.next_frame = cx.new_next_frame();
                }
            }
            // Check after the selected world was drawn: an old sky may still be bound
            // while the first requested studio prepares in the background.
            if scene.host == Some(self.pane) && !scene.studio_lit && scene.studio_keys(lighting).is_some()
                && scene.renderer.environment_ready() && !scene.renderer.environment_pending()
            {
                scene.studio_lit = true;
                log!("fab studio: the view is lit ({})", lighting.label());
            }
        } else {
            self.spare_scene_key = Some(scene_key);
            if self.cut_drawn != cut {
                self.move_cut(cx.cx, cut);
            }
        }
        if record_comp {
            self.comp_key = Some(comp_key);
            self.record_comp_pass(cx, rect, state, fab_state, light.as_ref().unwrap(), planes, print_frame.as_ref(), true);
            self.stats[2] += 1;
        } else if sample.is_some() {
            // Nothing changed: the next sample of the still view's average. With AO, sample 0
            // is recorded again too: seed 0 and no temporal history enter the still average.
            if let Some(offset) = self.still_aa.next_offset() {
                if offset != (0.0, 0.0) || self.ssao_on {
                    let dpi = cx.current_dpi_factor();
                    let size_px = ((rect.size.x * dpi) as f32, (rect.size.y * dpi) as f32);
                    let (mut fab_j, mut render_j) = (fab_state, render_state);
                    fab_j.projection = makepad_render::jittered_projection(fab_state.projection, offset, size_px);
                    render_j.projection = makepad_render::jittered_projection(render_state.projection, offset, size_px);
                    // each sample's own noise in the stochastic filters (a shadow's penumbra,
                    // the screen AO's taps)
                    self.noise_seed = self.still_aa.count();
                    {
                        let mut scene = scene_handle.borrow_mut();
                        let scene = &mut *scene;
                        let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
                        renderer.set_noise_seed(self.noise_seed);
                        renderer.set_csm_fit_projection(Some(render_state.projection));
                    }
                    self.record_scene_passes(cx, rect, state, fab_j, render_j, world.as_ref().unwrap(), light.unwrap(), &sdf_drawn, planes, cut, print_frame.as_ref(), force_frame.as_ref(), Some(self.noise_seed));
                    self.scene_jittered = offset != (0.0, 0.0);
                    {
                        let mut scene = scene_handle.borrow_mut();
                        let scene = &mut *scene;
                        let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
                        renderer.set_csm_fit_projection(None);
                        self.noise_seed = 0;
                        renderer.set_noise_seed(0);
                    }
                    self.record_comp_pass(cx, rect, state, fab_j, light.as_ref().unwrap(), planes, print_frame.as_ref(), false);
                    self.stats[1] += 1;
                    self.stats[2] += 1;
                } else if self.see_half {
                    // the frame on screen marched See-Through at half the pixels: the still
                    // view's first sample marches them all
                    self.record_comp_pass(cx, rect, state, fab_state, light.as_ref().unwrap(), planes, print_frame.as_ref(), false);
                    self.stats[2] += 1;
                }
                self.still_aa.add(cx, &self.comp_texture, rect.size);
                self.rig_vgrab_due = rig_mode().split_whitespace().any(|w| w == "vgrab");
                self.next_frame = cx.new_next_frame();
                if self.stats_log && self.still_aa.next_offset().is_none() {
                    log!("fab vp still: {} samples averaged", self.still_aa.count());
                }
            }
        }
        // Record after this draw's scene and sample: an orbit or jittered sample must put
        // its own aux visibility under the glow, with no lag frame.
        let live = BodyLiveKeys::of(cx.cx);
        let emitting = self.body_emitting || self.pipe_glow_key.is_some();
        let glow = GlowKey::of(emitting && self.traced.is_none(), live, self.aux_records, self.lit_records)
            .map(|mut key| { key.pipes = self.pipe_glow_key; key });
        if glow.is_some() && glow != self.glow_painted {
            let aux_state = self.aux_state.expect("an emitting surface has an aux recording");
            let window = cx.current_pass_id();
            self.glow_pass.set_pass_parent(cx.cx, window);
            self.glow_pass.set_size(cx, rect.size);
            self.glow_pass.set_color_texture(cx, &self.glow_texture, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
            self.glow_pass.clear_depth_texture(cx.cx);
            cx.begin_pass(&self.glow_pass, None);
            self.glow_pass.set_size(cx, rect.size);
            self.glow_pass.set_camera(cx.cx, aux_state.view, aux_state.projection);
            self.glow_list.begin_always(cx);
            let pass_size = cx.current_pass_size();
            cx.begin_root_turtle(pass_size, Layout::flow_overlay());
            {
                let mut scene = scene_handle.borrow_mut();
                let scene = &mut *scene;
                let renderer = Self::pane_renderer(scene.host == Some(self.pane), &mut scene.renderer, &mut self.review, &scene.model_id);
                // Emission is in display whites; the layer stores lit-texel units.
                let glow_gain = if self.hdr { 1.0 / display_exposure(renderer, &state.view, lighting, true) } else { 1.0 };
                self.glow_white = glow_gain;
                let cx3d = &mut Cx3d::new(cx.cx);
                cx3d.begin_scene_3d(aux_state);
                let previous = cx3d.set_scene_world_transform_3d(Mat4f::identity());
                self.draw_calls += scene.board_layer.draw_body_glow(cx3d, &self.aux_texture, glow_gain, &mut self.draw_body_glow, &self.board_moves, &state.elec_selected);
                self.draw_calls += scene.board_layer.draw_screens(cx3d, &self.aux_texture, glow_gain, &mut self.draw_body_screen, &self.board_moves, &state.elec_selected);
                if let (Some(_), Some(draw)) = (pipe_glow_key, self.draw_part_glow.as_mut()) {
                    self.draw_calls += scene.part_glow.draw(cx3d, &scene.batch_geometries, &self.elem_lut,
                        planes, cut, &self.aux_texture, glow_gain, draw);
                }
                // Raw surface writes first, then lens-only reflected light (alpha stays coverage).
                self.draw_calls += scene.board_layer.draw_body_glow_pool(cx3d, &self.aux_texture, &self.lit_texture, glow_gain, &mut self.draw_glow_pool, &self.board_moves, &mut self.pool_pane);
                if let Some(previous) = previous { let _ = cx3d.set_scene_world_transform_3d(previous); }
                cx3d.end_scene_3d();
            }
            cx.end_pass_sized_turtle();
            self.glow_list.end(cx);
            cx.end_pass(&self.glow_pass);
            self.bloom.flat_weights = true;
            self.bloom.threshold = self.glow_white;
            self.bloom.radius = None;
            let dt = self.bloom.frame_clock_dt(cx);
            self.bloom.run(cx, rect.size, &self.glow_texture, window, dt);
            if let Some(first) = self.bloom.first_pass_id() {
                self.glow_pass.set_pass_parent(cx.cx, first);
            }
            self.glow_painted = glow;
        } else if glow.is_none() {
            self.glow_painted = None;
        }
        self.body_live_painted = live;
        {
            let mut scene = scene_handle.borrow_mut();
            scene.sdf_drawn = sdf_drawn;
        }
        self.print_frame = print_frame;
        // Nothing changed since the passes were recorded and the view is at full quality: the
        // frame the person is looking at, once per plate content, is the project's picture.
        // Monitor draws a job, not the project.
        let content = (state.plate.revision, state.scene.generation);
        if self.pane == state.view.lead_plate() && !record_comp && self.picture_of != Some(content) && self.frame_ticket.is_none() && self.still_settled() && state.print.monitor_preview().is_none() {
            match self.picture.take(cx, &self.comp_pass, &self.comp_texture, rect.size) {
                Ok(Some(ticket)) => {
                    self.frame_ticket = Some(ticket);
                    self.frame_revision = content.0;
                    self.picture_of = Some(content);
                }
                // the shrink's draw or its shader is not there yet: a draw once it is takes it
                Ok(None) => self.picture_wait(cx.cx, self.picture.wait()),
                Err(e) => log!("fab vp: the settled frame could not be read back for the project's picture: {e:?}"),
            }
        }
        // rig: `vgrab`, the finished still view read back whole (the window's own frame of it,
        // without the widgets over the viewport), a band of rows a draw
        let finished = !record_comp && sample.is_none() && self.still_settled() && self.still_aa.next_offset().is_none();
        if self.rig_grab.active() && !finished {
            self.rig_grab.drop_grab();
            log!("fab vp vgrab: the view changed under the grab: dropped");
        }
        if self.rig_vgrab_due && finished && !self.rig_grab.active() {
            self.rig_grab.begin();
            self.rig_vgrab_due = false;
        }
        if self.rig_grab.active() {
            let texture = self.still_aa.output().cloned().unwrap_or_else(|| self.comp_texture.clone());
            match self.rig_grab.step(cx, &self.comp_pass, &texture, rect.size) {
                Ok(true) => {}
                Ok(false) => self.picture_wait(cx.cx, self.rig_grab.wait()),
                Err(e) => {
                    self.rig_grab.drop_grab();
                    log!("fab vp vgrab: not read back: {e:?}");
                }
            }
        }

        // ---- back into the 2D pass ----------------------------------------
        let ao_debug = self.debug_view as u32 == 3 && self.ssao_on && self.ssao_texture.is_some();
        self.draw_bg.draw_super.draw_vars.set_uniform(cx.cx, live_id!(u_ao_debug), &[if ao_debug { 1.0 } else { 0.0 }]);
        let normal_view = match self.debug_view as u32 {
            1 | 21..=26 => { self.draw_bg.set_scene_texture(&self.lit_texture); false }
            2 => { self.draw_bg.set_scene_texture(&self.aux_texture); false }
            3 if ao_debug => { self.draw_bg.set_scene_texture(self.ssao_texture.as_ref().unwrap()); false }
            _ => {
                self.draw_bg.set_scene_texture(self.still_aa.output().unwrap_or(&self.comp_texture));
                true
            }
        };
        self.draw_bg.mirror_x = if state.view.pane(self.pane).link.how() == Some(LinkHow::Through) { 1.0 } else { 0.0 };
        self.draw_bg.draw_abs(cx, rect);
        if normal_view && glow.is_some() {
            // Glow reads this draw's aux. Passes execute by dependency depth, not recording
            // order: finish the picture (including an in-flight average sample) before glow.
            // Dirtiness travels up to consumers only, so LED updates do not repaint the picture.
            // Preserve any picture/readback passes inserted after the composite this draw.
            let window = cx.current_pass_id();
            let mut picture_pass = self.comp_pass.draw_pass_id();
            while let CxDrawPassParent::DrawPass(parent) = cx.passes[picture_pass].parent {
                if parent == window { break; }
                picture_pass = parent;
            }
            cx.cx.set_pass_parent(picture_pass, self.glow_pass.draw_pass_id());
            // The window's list owns this attachment on every consuming draw, including
            // retained frames. Without it the whole layer is orphaned and stops painting.
            cx.make_child_pass_id(self.bloom.last_pass_id().unwrap_or_else(|| self.glow_pass.draw_pass_id()));
            let halo = self.bloom.bloom().filter(|_| self.bloom.levels() > 0);
            let vars = &mut self.draw_glow_over.draw_super.draw_vars;
            set_texture_named(vars, cx.cx, live_id!(picture), self.still_aa.output().unwrap_or(&self.comp_texture));
            set_texture_named(vars, cx.cx, live_id!(lit), &self.lit_texture);
            set_texture_named(vars, cx.cx, live_id!(glow), &self.glow_texture);
            set_texture_named(vars, cx.cx, live_id!(halo), halo.unwrap_or(&self.glow_texture));
            let f = if halo.is_some() { GLARE_FRACTION } else { 0.0 };
            vars.set_uniform(cx.cx, live_id!(u_glow), &[f, f / self.bloom.levels().max(1) as f32, self.glow_white, 0.0]);
            let dpi = cx.current_dpi_factor() as f32;
            let px = vec2((rect.size.x as f32 * dpi).max(1.0), (rect.size.y as f32 * dpi).max(1.0));
            vars.set_uniform(cx.cx, live_id!(u_texel), &[1.0 / px.x, 1.0 / px.y, px.x, px.y]);
            self.draw_glow_over.mirror_x = self.draw_bg.mirror_x;
            self.draw_glow_over.draw_abs(cx, rect);
        }
        // The rebuilding sweep: a quad over the retained picture, masked by the aux pass's
        // element ids. It animates in the window pass alone.
        if let Some((fx, span)) = self.rebuild_uniforms(cx, rect, state) {
            let v = &mut self.draw_rebuild.draw_super.draw_vars;
            v.set_texture(0, &self.aux_texture);
            v.set_uniform(cx.cx, live_id!(u_rebuild), &fx);
            v.set_uniform(cx.cx, live_id!(u_rebuild_span), &span);
            // Masked by the aux ids of the same picture: mirrored with it in a Through pane.
            self.draw_rebuild.mirror_x = self.draw_bg.mirror_x;
            self.draw_rebuild.draw_abs(cx, rect);
        }
        self.draw_crosshair(cx, state, rect);
        self.draw_control_ring(cx, state, rect);
        self.stats[0] += 1;
        self.draws += 1;
        if self.stats_log {
            let now = Cx::time_now();
            if now - self.stats_since >= 1.0 {
                let [prepass, main, filter, resolve, upsample] = self.ssao.stage_gpu_ms;
                log!("fab vp stats: {:.2}s draws {} scene records {} composite records {}; screen AO gpu {:.3} ms (prepass {prepass:.3} + main {main:.3} + filter {filter:.3} + resolve {resolve:.3} + upsample {upsample:.3})", now - self.stats_since, self.stats[0], self.stats[1], self.stats[2], self.ssao.gpu_ms());
                self.stats = [0; 3];
                self.stats_since = now;
            }
        }
        // The widget's area stays the one `walk_turtle_with_area` gave us.
        // Feeding the composite quad's area back in — and then handing THAT
        // to `set_pass_area` — made the pass rect and the widget rect two
        // different things, and the composite ended up painted at the window
        // origin. Nothing here needs a pass area: input is routed by
        // `self.area`.

        if self.sdf_prof_enabled {
            // A still view must record its passes to measure their GPU cost.
            self.force_record = true;
            self.next_frame = cx.new_next_frame();
        }
        if self.navigator.needs_frames() || self.issue_spot_drawn.is_some() {
            self.next_frame = cx.new_next_frame();
        }
        DrawStep::done()
    }
}

/// The box on the screen (points: min x, min y, max x, max y) of `discs` (plate mm: x, y, print z,
/// height, radius) as `proj` shows them: each disc's rim at its print z.
fn screen_box(proj: &ViewProjector, discs: &[[f32; 5]]) -> Option<[f64; 4]> {
    let m = |v: f32| v * 0.001;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for d in discs {
        for (dx, dy) in [(-d[4], 0.0), (d[4], 0.0), (0.0, -d[4]), (0.0, d[4])] {
            if let Some(p) = proj.project(vec3(m(d[0] + dx), m(d[1] + dy), m(d[2]))) {
                b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
            }
        }
    }
    (b[0] <= b[2]).then_some(b)
}

/// The lit branches' own beads, and what they were built of: the preview they light and the lit
/// branches' discs (the hovered one's, the picked one's) as they were. Built again only when the
/// preview shown is another or a lit branch is another or has changed, never because the trees
/// were given again (each new slice, part or plan gives the whole list).
struct BranchSpot {
    preview: Arc<crate::print::PrintPreview>,
    lit: [Option<Vec<[f32; 5]>>; 2],
    beads: RetainedInstances,
}

impl BranchSpot {
    /// Follow the branches lit now ([`FabViewport::branch_lit`]) on the preview shown: true when
    /// their beads were built.
    fn follow(spot: &mut Option<BranchSpot>, preview: Option<&Arc<crate::print::PrintPreview>>, lit: [Option<(&crate::print::PlateTree, u32)>; 2]) -> bool {
        let discs = lit.map(|b| b.map(|(t, low)| t.branch(low).iter().map(|&d| t.discs[d as usize]).collect::<Vec<_>>()));
        let Some(preview) = preview.filter(|_| discs.iter().any(Option::is_some)) else {
            *spot = None;
            return false;
        };
        if spot.as_ref().is_some_and(|s| Arc::ptr_eq(&s.preview, preview) && s.lit == discs) {
            return false;
        }
        *spot = Some(BranchSpot { preview: preview.clone(), lit: discs, beads: crate::print::trees::lit_beads(preview, lit) });
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_levels_paint_only_the_glow_layer() {
        use fab_elec_view::live::{LiveRows, LIVE_ROW_W, PART_OFF};
        let absent = BodyLiveKeys::of_sim(None);
        assert_eq!(absent, BodyLiveKeys::default(), "no sim has no live keys");
        assert_eq!(GlowKey::of(true, absent, 1, 1), None, "no sim paints no glow layer");
        let mut sim = SimLive::default();
        let initial = BodyLiveKeys::of_sim(Some(&sim));
        assert_ne!(initial.rows, absent.rows, "a host exposes numbering before its first texture");
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let mut rows = LiveRows::with_off(PART_OFF);
        rows.resize(1);
        sim.parts = Some(rows.texture(&mut cx).clone());
        let bound = BodyLiveKeys::of_sim(Some(&sim));
        assert_ne!(bound.rows, initial.rows, "the first parts texture changes the row binding");
        assert_eq!(bound.lights, initial.lights, "binding a texture is independent of LED levels");
        assert_eq!(GlowKey::of(true, bound, 1, 1), None, "all-dark LEDs paint no glow layer");
        sim.playing = true;
        sim.emission = 0.1;
        rows.set(0, [0.7, sim.emission, 0.0, 0.0]);
        sim.parts = Some(rows.texture(&mut cx).clone());
        sim.lights += 1;
        let playing = BodyLiveKeys::of_sim(Some(&sim));
        assert_ne!(playing, bound, "playback moves the live key");
        assert_eq!(playing.rows, bound.rows, "playback preserves row identity");
        let mut key = GlowKey::of(true, playing, 1, 1);
        assert!(key.is_some(), "a lit emitting lens paints the glow layer");
        for level in [0.05, 1.0, 0.25] {
            rows.set(0, [0.7, level, 0.0, 0.0]);
            sim.parts = Some(rows.texture(&mut cx).clone());
            sim.emission = level;
            sim.lights += 1;
            sim.t += 0.01;
            let live = BodyLiveKeys::of_sim(Some(&sim));
            assert_eq!(live.rows, playing.rows, "value publications preserve texture and numbering");
            assert!(live.playing, "the changing LED remains in playback");
            let plan = FramePlan::of(false, false, true, true);
            assert!(!plan.record_scene, "LED levels never record scene passes");
            assert!(!plan.record_comp, "LED levels never record the composite");
            assert!(!plan.restart_still, "LED levels never restart the still average");
            assert!(plan.wants_sample, "a settled view still requests samples while LEDs change");
            let published = GlowKey::of(true, live, 1, 1);
            assert_ne!(published, key, "each LED publication moves the glow key");
            assert_eq!(published, GlowKey::of(true, live, 1, 1), "unchanged inputs retain the glow layer");
            let sampled = GlowKey::of(true, live, 2, 1);
            assert_ne!(sampled, published, "a new aux sample moves the glow key");
            assert_ne!(GlowKey::of(true, live, 2, 2), sampled, "a new lit recording moves the glow key");
            key = published;
        }
        let live = BodyLiveKeys::of_sim(Some(&sim));
        assert_eq!(GlowKey::of(false, live, 1, 1), None, "without an emitting lens there is no glow layer");
        sim.playing = false;
        sim.scrub = Some(0.0);
        let paused = BodyLiveKeys::of_sim(Some(&sim));
        assert_ne!(paused, live, "a pause moves the live key: the view looks once");
        assert_eq!(GlowKey::of(true, paused, 1, 1), key, "transport alone does not change glow");
        rows.resize(LIVE_ROW_W + 1);
        sim.parts = Some(rows.texture(&mut cx).clone());
        let grown = BodyLiveKeys::of_sim(Some(&sim));
        assert_ne!(grown.rows, paused.rows, "texture growth changes the row binding");
        let grown_key = GlowKey::of(true, grown, 1, 1);
        assert_ne!(grown_key, key, "a new parts texture moves the glow key");
        assert_eq!(grown.lights, paused.lights, "texture growth leaves the light counter alone");
        // the default index carries the empty-generation sentinel: renumber by wrapping
        let renumbered = sim.index.generation.wrapping_add(1);
        Arc::make_mut(&mut sim.index).generation = renumbered;
        assert_ne!(GlowKey::of(true, BodyLiveKeys::of_sim(Some(&sim)), 1, 1), grown_key,
            "row renumbering moves the glow key even with the same texture");
        sim.emission = 0.0;
        assert_eq!(GlowKey::of(true, BodyLiveKeys::of_sim(Some(&sim)), 1, 1), None,
            "clearing the last lit LED removes the glow layer");
        for (scene_changed, comp_changed) in [(true, false), (false, true)] {
            let plan = FramePlan::of(scene_changed, comp_changed, true, true);
            assert_eq!(plan.record_scene, scene_changed, "only a scene change records scene passes");
            assert!(plan.record_comp, "a scene or composite change records the composite");
            assert!(plan.restart_still, "a scene or composite change restarts the still average");
            assert!(!plan.wants_sample, "a changed picture cannot supply a still sample");
        }
        assert!(!FramePlan::of(false, false, false, true).wants_sample,
            "an unsettled view requests no still sample");
        assert!(!FramePlan::of(false, false, true, false).wants_sample,
            "a completed still average requests no more samples");
    }

    /// rig: the 9x reference's nine crops (`crop<x><y>`) tile the view: each third, x from the
    /// left and y from the top, fills the whole viewport along the same rays, so the nine
    /// pictures stitched in that order are the view at three times its pixels.
    #[test]
    fn the_nine_crops_tile_the_view() {
        let projection = Camera::default().projection(1.5);
        let unproject = projection.invert();
        for y in 0..3u32 {
            for x in 0..3u32 {
                let crop = rig_crop_projection(projection, (x, y));
                // the third's middle and corners, in the whole view's picture (y up)
                let (cx, cy) = (-1.0 + (2.0 * x as f32 + 1.0) / 3.0, 1.0 - (2.0 * y as f32 + 1.0) / 3.0);
                for (u, v) in [(0.0, 0.0), (-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                    let q = crop.transform_vec4(unproject.transform_vec4(vec4(cx + u / 3.0, cy + v / 3.0, 0.5, 1.0)));
                    assert!((q.x / q.w - u).abs() < 1e-4 && (q.y / q.w - v).abs() < 1e-4, "crop{x}{y}: ({u}, {v}) of the third reads ({}, {}) in the crop", q.x / q.w, q.y / q.w);
                }
            }
        }
    }

    /// The person's view while a plan goes on: a branch picked, another hovered. Each new slice,
    /// part or plan gives the whole list of trees again, as new values of the same trees: that
    /// builds the lit branches' beads no more (the trees lit built theirs every frame). Only
    /// another branch lit, a lit branch that changed or another preview shown builds them, once;
    /// with none lit there are none.
    #[test]
    fn the_lit_branches_beads_are_built_once_however_often_the_trees_are_given() {
        // A trunk of `n` discs at `x`, its one tip on its top disc.
        let tree = |x: f32, n: u32| {
            let discs = (0..n).map(|k| [x, 0.0, 0.2 * (k + 1) as f32, 0.2, 1.0]).collect();
            let links: Vec<[u32; 2]> = (1..n).map(|k| [k - 1, k]).collect();
            crate::print::PlateTree::new("lattice_cube/lattice".into(), vec![[x as f64, 0.0, 0.2 * n as f64]], vec![n - 1], discs, &links)
        };
        let given = |grown: u32| vec![tree(1.0, 10), tree(2.0, 10), tree(3.0, grown)];
        let (shown, other) = (crate::print::follow::tests::preview_of(4), crate::print::follow::tests::preview_of(4));
        let mut spot = None;
        let builds = |spot: &mut Option<BranchSpot>, frames: usize, preview: &Arc<crate::print::PrintPreview>, lit: [Option<usize>; 2], grown: u32| {
            (0..frames).filter(|_| {
                let trees = given(grown);
                BranchSpot::follow(spot, Some(preview), lit.map(|i| i.map(|i| (&trees[i], 0))))
            }).count()
        };
        assert_eq!(builds(&mut spot, 100, &shown, [Some(0), Some(2)], 10), 1, "the same trees given every frame");
        assert_eq!(builds(&mut spot, 100, &shown, [Some(1), Some(2)], 10), 1, "another branch hovered");
        assert_eq!(builds(&mut spot, 100, &shown, [Some(1), Some(2)], 12), 1, "the picked branch grew");
        assert_eq!(builds(&mut spot, 100, &other, [Some(1), Some(2)], 12), 1, "another preview shown");
        assert_eq!(builds(&mut spot, 100, &other, [None, None], 12), 0, "none lit");
        assert!(spot.is_none(), "no beads with no branch lit");
    }

    #[test]
    fn studio_does_not_request_a_star_map() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        let state = AppState::default();
        let lighting = state.view.lighting_for(&state.ui);
        assert_eq!(lighting, Lighting::Studio);
        let mut scene = state.scene_gpu.borrow_mut();
        scene.start_star_map(&mut cx, lighting);
        assert!(!scene.star_map_asked, "Studio must leave the first sky request pending");
        assert!(scene.star_map_task.is_none(), "Studio must not submit star-map work");
    }

    #[test]
    fn a_section_box_becomes_six_inward_planes() {
        let s = SectionState {
            enabled: true,
            planes: Vec::new(),
            boxed: Some(Aabb {
                min: vec3(-1.0, -2.0, 0.0),
                max: vec3(3.0, 4.0, 5.0),
            }),
            caps: true,
        };
        let planes = section_planes(&s);
        let p = planes.as_slice();
        assert_eq!(p.len(), 6);
        // The box centre is kept by every plane; a point outside is not.
        let inside = vec3(1.0, 1.0, 2.5);
        for pl in p {
            let d = pl[0] * inside.x + pl[1] * inside.y + pl[2] * inside.z + pl[3];
            assert!(d >= 0.0, "centre rejected by {pl:?} ({d})");
        }
        let outside = vec3(9.0, 1.0, 2.5);
        let kept = p
            .iter()
            .all(|pl| pl[0] * outside.x + pl[1] * outside.y + pl[2] * outside.z + pl[3] >= 0.0);
        assert!(!kept, "a point past max.x must be cut away");
    }

    #[test]
    fn a_test_prints_cut_is_its_box_as_turned() {
        let cube = TestPrintView {
            name: "T".into(),
            object: "o".into(),
            center: [100.0, 50.0, 30.0],
            size: [20.0, 10.0, 40.0],
            turn: [0.0, 0.0, 90.0],
            pose: None,
            lay: None,
            on: true,
            brim: false,
        };
        let planes = cube_planes(&cube);
        let keeps = |mm: [f32; 3]| planes.as_slice().iter().all(|p| p[0] * mm[0] * 0.001 + p[1] * mm[1] * 0.001 + p[2] * mm[2] * 0.001 + p[3] >= 0.0);
        assert!(keeps([100.0, 50.0, 30.0]), "its centre");
        // Turned a quarter about Z, its 20 mm side lies along Y and its 10 mm side along X.
        assert!(keeps([100.0, 59.0, 30.0]) && keeps([104.0, 50.0, 49.0]), "inside, along its own axes");
        assert!(!keeps([109.0, 50.0, 30.0]), "past its 10 mm side, which a box not turned would keep");
        assert!(!keeps([100.0, 61.0, 30.0]) && !keeps([100.0, 50.0, 51.0]), "past its other sides");
        // The lit meshes keep the same box, in their own frame.
        let rows = lit_rows(&cube);
        let lit_keeps = |mm: [f32; 3]| {
            let p = pack::to_render(vec3(mm[0] * 0.001, mm[1] * 0.001, mm[2] * 0.001));
            rows.iter().all(|r| (r[0] * p.x + r[1] * p.y + r[2] * p.z + r[3]).abs() <= 1.0)
        };
        for mm in [[100.0, 50.0, 30.0], [100.0, 59.0, 30.0], [104.0, 50.0, 49.0], [109.0, 50.0, 30.0], [100.0, 61.0, 30.0], [100.0, 50.0, 51.0]] {
            assert_eq!(lit_keeps(mm), keeps(mm), "{mm:?}");
        }
    }

    #[test]
    fn a_disabled_section_clips_nothing() {
        let s = SectionState {
            enabled: false,
            planes: vec![SectionPlane {
                plane: Plane::from_point_normal(vec3(0.0, 0.0, 0.0), vec3(0.0, 0.0, 1.0)),
                enabled: true,
            }],
            boxed: None,
            caps: true,
        };
        assert!(section_planes(&s).as_slice().is_empty());
    }

    #[test]
    fn both_cameras_frame_the_same_points() {
        // The aux (Z up) and lit (Y up) passes cover the same pixels: the
        // composed view-projection over a Fab point must be identical
        // whichever space it goes through.
        let cam = Camera::default();
        let aspect = 1.6f32;
        let view_fab = cam.view();
        let view_render = Mat4f::look_at(
            pack::to_render(cam.eye),
            pack::to_render(cam.target),
            pack::to_render(cam.up),
        );
        for p in [
            vec3(0.0, 0.0, 0.0),
            vec3(5.0, 3.5, 2.5),
            vec3(-12.0, 7.0, 9.0),
        ] {
            let a = Mat4f::mul(&cam.projection(aspect), &view_fab)
                .transform_vec4(vec4(p.x, p.y, p.z, 1.0));
            let r = pack::to_render(p);
            let b = Mat4f::mul(&cam.projection(aspect), &view_render)
                .transform_vec4(vec4(r.x, r.y, r.z, 1.0));
            for (x, y) in [(a.x, b.x), (a.y, b.y), (a.z, b.z), (a.w, b.w)] {
                assert!((x - y).abs() < 1e-3, "{p:?}: {x} vs {y}");
            }
        }
    }

    #[test]
    fn solid_mode_uses_subtle_cavity() {
        let mut vs = ViewportState::default();
        vs.shading = Shading::Solid;
        assert!(vs.overlays.cavity);
        let f = shading_overlay_flags(&vs);
        assert!((f[1] - 0.25).abs() < 1e-6, "cavity strength {f:?}");
    }

    #[test]
    fn a_balanced_light_is_white_at_its_own_luminance() {
        let warm = vec3(1.0, 0.86, 0.71);
        let g = white_balance(warm);
        let out = [warm.x * g[0], warm.y * g[1], warm.z * g[2]];
        let l = 0.2126 * warm.x + 0.7152 * warm.y + 0.0722 * warm.z;
        assert!(out.iter().all(|c| (c - l).abs() < 1.0e-5), "{out:?} vs {l}");
        assert_eq!(white_balance(vec3(0.0, 0.0, 0.0)), [1.0; 3], "a dark key leaves the light as it is");
    }

    #[test]
    fn realtime_uses_renderer_occlusion_without_cad_double_darkening() {
        let mut vs = ViewportState::default();
        vs.shading = Shading::Realtime;
        let f = shading_overlay_flags(&vs);
        assert_eq!(f[1], 0.0, "CAD cavity must be off in Realtime: {f:?}");
        assert!(wants_ssao(&vs), "Realtime's creases are the screen AO's");
        vs.shading = Shading::Solid;
        assert!(!wants_ssao(&vs), "Solid's are its cavity's alone");
    }
}

/// A part's rebuilding effect in the view.
#[derive(Clone, Debug)]
struct RebuildFx {
    object: String,
    started: f64,
    ended: Option<f64>,
}

/// The part under a ray (printer frame, metres) as `pane` shows it: parts moved by Mechanics are hit where they show;
/// parts the SDF engine draws are hit on their exact fields (the surface shown), not their meshes.
pub fn pick_ray(state: &AppState, pane: PaneId, ray: &crate::model::query::Ray) -> Option<RayHit> {
    pick_ray_where(state, pane, ray, None)
}

/// [`pick_ray`] as `pane` shows the parts: at its poses, without what it leaves out.
pub fn pick_ray_in(state: &AppState, pane: PaneId, ray: &crate::model::query::Ray) -> Option<RayHit> {
    pick_ray_where(state, pane, ray, None)
}

/// [`pick_ray`] on the part surface `keep` holds of (the hit point, metres): a slice's shown
/// layers, the parts above its slider's cut left out. A mesh is hit on its first such surface
/// along the ray, a field on its first surface when `keep` holds there.
fn pick_ray_where(state: &AppState, pane: PaneId, ray: &crate::model::query::Ray, keep: Option<&dyn Fn(Vec3f) -> bool>) -> Option<RayHit> {
    let ray = *ray;
    let field = state.sdf.as_ref().map(|l| l.borrow());
    let poses = state.pane_poses(pane);
    let shown = |id| state.is_visible(id) && !state.plate.is_context(id) && !state.is_ghost_under(poses, id) && !state.case_hidden_in(pane, id);
    // A marked part is drawn from its installed step mesh, including when the
    // final part also has a field registered with the viewport.
    let step = state.steps.shown.and(state.steps.part.as_ref()).and_then(|id| state.plate.parts.iter().find(|p| &p.object == id)?.element);
    let field_hit = field.as_ref().and_then(|l| {
        let o = [ray.origin.x as f64 * 1000.0, ray.origin.y as f64 * 1000.0, ray.origin.z as f64 * 1000.0];
        let d = [ray.dir.x as f64, ray.dir.y as f64, ray.dir.z as f64];
        let (element, h) = l.pick(state, o, d, &|id| Some(id) != step && shown(id))?;
        let t = (h.t * 0.001) as f32;
        let normal = vec3(h.normal[0] as f32, h.normal[1] as f32, h.normal[2] as f32);
        // facing the ray origin, as the mesh pick's
        let normal = if normal.dot(ray.dir) > 0.0 { -normal } else { normal };
        // no triangle: the hit is on the field
        Some(RayHit { element, batch: u32::MAX, triangle: u32::MAX, t, point: ray.origin + ray.dir * t, normal, bary: [0.0, 0.0] })
    }).filter(|h| keep.is_none_or(|keep| keep(h.point)));
    let pickable = |id| shown(id) && (Some(id) == step || !field.as_ref().is_some_and(|l| l.picks(state, id)));
    let nearer = |a: Option<RayHit>, b: Option<RayHit>| match (a, b) {
        (Some(a), Some(b)) => Some(if b.t < a.t { b } else { a }),
        (a, b) => a.or(b),
    };
    let best = pick_mesh(state, pane, &ray, &pickable, keep);
    nearer(best, field_hit)
}

/// The mesh pick of [`pick_ray_where`]: parts `pane` shows moved (its pose set's, Mechanics', a
/// placement preview's or a resin slice's lift) hit where they show.
fn pick_mesh(state: &AppState, pane: PaneId, ray: &crate::model::query::Ray, pickable: &dyn Fn(ElementId) -> bool, keep: Option<&dyn Fn(Vec3f) -> bool>) -> Option<RayHit> {
    let ray = *ray;
    let ss = &state.scene_state;
    let cast = |ray: &crate::model::query::Ray, visible: &dyn Fn(ElementId) -> bool, accept: Option<&dyn Fn(Vec3f) -> bool>| {
        state.scene.bvh.raycast_opt(&state.scene.batches, ray, &crate::model::bvh::PickOptions { accept, ..crate::model::bvh::PickOptions::new(visible) })
    };
    let still = match state.pane_poses(pane) {
        PanePoses::Stage => ss.moves.is_none() && ss.nudge.is_none() && ss.lift.is_none(),
        PanePoses::Set(set) => state.pose_set(set).is_empty(),
    };
    if still {
        return cast(&ray, pickable, keep);
    }
    let posed = state.posed_in(pane);
    // Moved parts: each one's hit is the ray taken back into where it rests.
    let mut best = cast(&ray, &|id| pickable(id) && state.pose_in(pane, id).is_none(), keep);
    for (id, a) in posed.iter() {
        if !pickable(*id) {
            continue;
        }
        let rt = |v: Vec3f| vec3(a[0][0] * v.x + a[1][0] * v.y + a[2][0] * v.z, a[0][1] * v.x + a[1][1] * v.y + a[2][1] * v.z, a[0][2] * v.x + a[1][2] * v.y + a[2][2] * v.z);
        let r = |v: Vec3f| vec3(a[0][0] * v.x + a[0][1] * v.y + a[0][2] * v.z, a[1][0] * v.x + a[1][1] * v.y + a[1][2] * v.z, a[2][0] * v.x + a[2][1] * v.y + a[2][2] * v.z);
        let t = vec3(a[0][3], a[1][3], a[2][3]);
        let local = crate::model::query::Ray { origin: rt(ray.origin - t), dir: rt(ray.dir) };
        let keep_here = |p: Vec3f| keep.is_none_or(|keep| keep(r(p) + t));
        if let Some(mut h) = cast(&local, &|e| e == *id, keep.map(|_| &keep_here as &dyn Fn(Vec3f) -> bool)) {
            if best.as_ref().map_or(true, |b| h.t < b.t) {
                h.point = r(h.point) + t;
                h.normal = r(h.normal);
                best = Some(h);
            }
        }
    }
    best
}

/// Fab's Z-up scene frame turned into the lit model's Y-up frame
/// ([`pack::to_render`]: (x, y, z) -> (x, z, -y)), column-major.
fn render_from_fab() -> Mat4f {
    Mat4f { v: [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0] }
}

/// The menu and document camera commands: the active Plate carries them out once.
fn camera_command(action: &ShellAction) -> bool {
    matches!(action,
        ShellAction::FrameAll | ShellAction::FrameOpening(_) | ShellAction::RestoreView { camera: true, .. }
        | ShellAction::SetWorkspace(_) | ShellAction::OrbitAroundSelection | ShellAction::FrameSelected
        | ShellAction::FrameBox(_) | ShellAction::FrameFrom { .. } | ShellAction::PresetView(_)
        | ShellAction::ToggleOrtho | ShellAction::OrbitBy(_, _)
    )
}

/// The camera commands a person gives (the View menu, the view pie, the gizmo, the palette):
/// they act on the view last used ([`ViewportState::camera_pane`]), the Product view too.
fn person_camera_command(action: &ShellAction) -> bool {
    matches!(action,
        ShellAction::FrameAll | ShellAction::OrbitAroundSelection | ShellAction::FrameSelected
        | ShellAction::PresetView(_) | ShellAction::ToggleOrtho | ShellAction::OrbitBy(_, _)
    )
}

/// A camera jump: the screen AO's history starts over, on every pane (the linked panes jump with
/// their source).
fn camera_jump(action: &ShellAction) -> bool {
    matches!(action,
        ShellAction::FrameAll | ShellAction::FrameOpening(_) | ShellAction::RestoreView { camera: true, .. }
        | ShellAction::FrameSelected | ShellAction::FrameBox(_) | ShellAction::FrameFrom { .. }
        | ShellAction::PresetView(_) | ShellAction::ToggleOrtho
    )
}

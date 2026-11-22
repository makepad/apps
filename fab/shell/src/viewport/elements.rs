//! The per-element lookup texture — how hide / isolate / select / a dragged
//! part's preview reach the GPU without touching a single vertex.
//!
//! ## Layout (the contract every consumer reads)
//!
//! * **Vertex lane.** The element index rides the `GameMeshVertexAo.ao_uv`
//!   slot (`viewport::pack::pack_element`): unorm16x2, low half in the first
//!   axis, so 2³² elements fit. Fab models carry no lightmap chart, which is
//!   what makes that lane free — no new POD, no change to
//!   `MODEL_VERTEX_FLOATS`, no second vertex format in `libs/render`.
//! * **Lookup texture.** `VecRGBAf32`, `LUT_WIDTH` texels per row, [`STRIDE`]
//!   (4) texels per element, row-major by element index:
//!   | texel | channels | meaning |
//!   |---|---|---|
//!   | 0 | `r` | state: `0` hidden, `1` visible, `2` selected, `3` active |
//!   | 0 | `g b a` | model-space (render, Y-up) offset added after the rotation |
//!   | 1..3 | `r g b` | the rows of a model-space (render) rotation: a rigid move |
//!   | 2 | `a` | a part drawn as a ghost (its eye is off): its state `1..3`, else `0`; the state in `r` is `0`; [`ARRIVING`] more for a part on its way in |
//!   | 3 | `a` | that ghost's colour, 8-bit sRGB packed `r * 65536 + g * 256 + b`; [`UNCUT`] on what no plane cuts |
//!   A dragged part's preview is the translation-only case; Mechanics writes
//!   every moving part's pose.
//! * **Uniform.** `elem_ctl = (width, height, STRIDE, enabled)` on the static
//!   shader; `0` in `w` disables the whole lookup.
//!
//! The static-mesh shader reads `r < 0.5` as "collapse this vertex to the
//! model origin", which makes the whole triangle zero-area — a hidden
//! element costs no fragments. The composite pass reads the same texture for
//! the selection outline, so the lit image and the outline can never
//! disagree about what is selected.
//!
//! An element the SDF engine draws (`viewport::sdf`) keeps its state negated:
//! raster passes read `r < 0.5` and collapse its mesh, the composite reads
//! `abs(r)` and outlines it like any other part.
//!
//! The renderer's sun cascades are drawn again only when what their casters
//! read here changes ([`ElementLut::casters`]): a selection, or a part the
//! SDF engine draws being dragged, keeps them.
//!
//! The table is written again when what it holds changes: a `SceneState::revision` (a
//! selection, or Mechanics' poses, every frame a part moves), the SDF parts, the floor. Only its
//! rows go up then. The plate's paint atlas under it ([`super::paint`]) is packed and uploaded
//! again only when what it is packed of changes ([`AtlasKey`]).

use crate::api::*;
use crate::viewport::pack::to_render;
use makepad_widgets_core::*;

pub const LUT_WIDTH: usize = 256;
/// Texels per element.
pub const STRIDE: usize = 4;
/// In texel 3's `a`: an element no plane cuts. A test print is cut out of the plate, not out of
/// the printer that stands under it.
const UNCUT: f32 = -1.0;

pub const STATE_HIDDEN: f32 = 0.0;
pub const STATE_VISIBLE: f32 = 1.0;
pub const STATE_SELECTED: f32 = 2.0;
pub const STATE_ACTIVE: f32 = 3.0;
/// Added to a ghost's state for a part on its way in (a file dragged over the window): the
/// composite shows more of it and draws its edges.
pub const ARRIVING: f32 = 4.0;

/// What the paint atlas in the texture is packed of: the scene's generation (how many table rows
/// come first), the paint's revision, the plate's revision and the Force overlay.
type AtlasKey = (u64, u64, u64, Option<(u64, usize, bool)>);

/// The shared visibility/pose inputs of the element table and the retained glass layer.
/// Scene generation also versions its materials and aux uploads; plate revision versions
/// the insert links and display colours. Neither lane needs another revision counter.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct ViewKey {
    generation: u64,
    revision: u64,
    plate: u64,
    slice: bool,
    force: Option<(u64, usize, bool)>,
    floor: bool,
    /// The parts other layers draw in their place: See-Through's and the cased boards'.
    see: u64,
    glass: bool,
    /// The items printed into their parts that the Sliced view's top layer has not reached the
    /// pause for ([`AppState::embed_waits`]): moving the top layer shows or hides them.
    waits: u64,
    /// The pane's pose set and its revision; the stage's poses move `revision` above.
    pub(super) poses: Option<(PoseSetId, u64)>,
    /// The printed keycaps and knobs of the parts the Electronics views selected
    /// ([`elec_selected_caps`]): they are ringed as the active selection.
    caps: u64,
    /// Whether the pane's Case toggle leaves the device's case out, and which case set (a new
    /// one each time its parts change).
    pub(super) case: (bool, usize),
}

pub(super) fn view_key(state: &AppState, pane: PaneId) -> ViewKey {
    let see = state.view.see.drawn.iter().chain(&state.drawn_by_link).fold(state.view.see.drawn.len() as u64 ^ (state.drawn_by_link.len() as u64) << 32, |h, e| h.wrapping_mul(0x100_0000_01b3) ^ e.0 as u64);
    ViewKey {
        generation: state.scene.generation,
        revision: state.scene_state.revision,
        plate: state.plate.revision,
        slice: state.parts_under_slice(),
        force: super::force::overlay_key(state),
        floor: state.floor_shown_in(pane),
        see,
        glass: state.view.glass,
        // O(embedded items × pauses) per frame and pane: a handful, none on most plates.
        poses: state.poses_key(state.pane_poses(pane)),
        case: (
            state.view.panes.get(pane.0 as usize).and_then(Option::as_ref).is_some_and(|p| p.case_hidden),
            std::sync::Arc::as_ptr(&state.device.case) as usize,
        ),
        waits: state.plate.parts.iter().filter(|p| p.embed).filter_map(|p| p.element).filter(|&e| state.embed_waits(e)).fold(0, |h: u64, e| h.wrapping_mul(0x100_0000_01b3) ^ (e.0 as u64 + 1)),
        caps: elec_selected_caps(state).fold(0, |h: u64, e| h.wrapping_mul(0x100_0000_01b3) ^ (e.0 as u64 + 1)),
    }
}

/// The plate parts bound to a live control (a printed keycap on its switch, a knob on its pot)
/// whose part the Electronics views selected: a closed case hides the part, its cap shows.
/// O(bound parts x selected), a handful; per frame and pane through [`view_key`].
fn elec_selected_caps(state: &AppState) -> impl Iterator<Item = ElementId> + '_ {
    let selected = &state.elec_selected;
    state.part_presses.iter().map(|p| (p.element, p.comp)).chain(state.part_turns.iter().map(|p| (p.element, p.comp)))
        .filter(move |(_, comp)| selected.contains(comp)).map(|(element, _)| element)
}

/// What a [`ElementLut::sync`] wrote again.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LutSync {
    /// Nothing changed.
    Kept,
    /// The element table alone, and only its rows went up: the atlas stays as packed.
    Table,
    /// The table and the paint atlas, the whole texture.
    All,
}

/// Owns the texture and the revision it was built for.
pub struct ElementLut {
    texture: Option<Texture>,
    width: usize,
    height: usize,
    /// The table's rows, before the atlas.
    table_rows: usize,
    /// What the atlas in the texture was packed of.
    atlas: Option<AtlasKey>,
    /// Each painted element and its volume's header texel, from that pack.
    painted: Vec<(usize, f32)>,
    /// The shared view key, SDF revision and paint revision of the data on the GPU.
    built: Option<(ViewKey, u64, u64)>,
    /// See [`Self::casters`].
    casters: u64,
    /// Opaque bounds and moved mesh reach, accumulated with the table, never per camera frame.
    shown_bounds: (Aabb, Aabb),
    /// What the atlas in the texture was packed coarser of to fit it (empty: nothing).
    coarser: String,
}

impl Default for ElementLut {
    fn default() -> Self {
        ElementLut {
            texture: None,
            width: 0,
            height: 0,
            table_rows: 0,
            atlas: None,
            painted: Vec::new(),
            built: None,
            casters: 0,
            shown_bounds: (aabb_empty(), aabb_empty()),
            coarser: String::new(),
        }
    }
}

impl ElementLut {
    pub fn texture(&self) -> Option<&Texture> {
        self.texture.as_ref()
    }

    /// `(width, height)` for the `elem_ctl` uniform.
    pub fn size(&self) -> (f32, f32) {
        (self.width as f32, self.height as f32)
    }

    /// Whether the data on the GPU draws the floor ([`AppState::floor_shown`]); `None` before any.
    pub fn floor_drawn(&self) -> Option<bool> {
        self.built.map(|key| key.0.floor)
    }

    /// A key of what a raster caster reads in the data on the GPU: which elements the mesh
    /// draws (`r >= 0.5`) and their poses. Selection states and the poses of hidden or
    /// field-drawn elements are left out.
    pub fn casters(&self) -> u64 {
        self.casters
    }

    /// Opaque parts where they show, and the moved subset drawn by meshes, for the cascades.
    pub(super) fn shown_bounds(&self) -> (Aabb, Aabb) {
        self.shown_bounds
    }

    /// Rebuild iff the scene or its state moved: what changed on the GPU copy.
    /// The plate's paint atlas goes in after the table (`super::paint`): each painted element's
    /// texel 1 `.w` is its volume's header texel, stored as the atlas stores texel numbers
    /// (`fab_paint_field::bake::texel_number`, above zero; 0 for none). A change the atlas is not
    /// packed of (a pose, a selection) writes the table's rows alone and uploads only them.
    pub fn sync(&mut self, cx: &mut Cx, state: &AppState, pane: PaneId, sdf: &[ElementId], sdf_revision: u64, paint: &mut super::paint::PaintLayer) -> LutSync {
        let key = (view_key(state, pane), sdf_revision, paint.revision());
        if self.built == Some(key) && self.texture.is_some() {
            return LutSync::Kept;
        }
        let n = state.scene.elements.len().max(1) * STRIDE;
        let rows_at = |width: usize| (n + width - 1) / width;
        let atlas: AtlasKey = (state.scene.generation, paint.revision(), state.plate.revision, super::force::overlay_key(state));
        if let Some(t) = self.texture.clone().filter(|_| self.atlas == Some(atlas) && self.width >= LUT_WIDTH && self.table_rows == rows_at(self.width)) {
            let (width, table_rows) = (self.width, self.table_rows);
            let mut data = t.take_vec_f32(cx);
            self.write_table(&mut data[..table_rows * width * 4], state, pane, sdf);
            t.put_back_vec_f32(cx, data, Some(RectUsize::new(PointUsize::new(0, 0), SizeUsize::new(width, table_rows))));
            self.built = Some(key);
            return LutSync::Table;
        }
        // Rows of LUT_WIDTH texels, twice as wide while the table and the atlas after it would
        // stand taller than the device's largest texture (a fine Force overlay's atlas outgrows
        // 256 texels a row: 16733 rows at 0.265 mm cells, past Metal's 16384); the shaders read
        // the width from `elem_ctl`. An atlas no width holds packs again coarser, as little as
        // fits that texture (`PaintLayer::pack_into`'s room), and the log says what and why.
        // The width comes from the atlas's size (`PaintLayer::pack_texels`), so it packs once.
        let side = cx.gpu_info().max_texture_size.max(LUT_WIDTH);
        let mut width = LUT_WIDTH;
        let mut texels = paint.pack_texels(state, width);
        while rows_at(width) + texels / width > side && width * 2 <= side {
            width *= 2;
            texels = paint.pack_texels(state, width);
        }
        let table_rows = rows_at(width);
        let table = table_rows * width * 4;
        let fits = table_rows + texels / width <= side;
        // The texture's data is written once: the table's rows, the atlas packed onto them
        // (`PaintLayer::pack_onto`). Its own data when its size holds (its pages already in), else
        // new, reserved whole.
        let same = fits && self.texture.is_some() && self.width == width && self.height == table_rows + texels / width;
        let mut data = match self.texture.as_ref().filter(|_| same) {
            Some(t) => {
                let mut d = t.take_vec_f32(cx);
                d.clear();
                d
            }
            None => Vec::with_capacity(table + if fits { texels * 4 } else { 0 }),
        };
        data.resize(table, 0.0);
        let (pack, height) = if fits {
            let pack = paint.pack_onto(state, data, 0, width, None);
            let height = pack.data.len() / (width * 4);
            (pack, height)
        } else {
            let room = side.saturating_sub(table_rows) * width;
            let coarse = paint.pack_onto(state, data, 0, width, Some(room));
            let height = coarse.data.len() / (width * 4);
            if coarse.coarser != self.coarser {
                log!("fab viewport: the paint atlas ({:.1}M texels) does not fit the device's largest texture ({side} a side, {:.1}M texels): drawn coarser, {} ({:.1}M texels)", texels as f64 / 1e6, (side * side) as f64 / 1e6, coarse.coarser, (coarse.data.len() - table) as f64 / 4e6);
            }
            if height <= side {
                (coarse, height)
            } else {
                // (past every halving: a plate of millions of painted parts)
                log!("fab viewport: the paint atlas does not fit the device's largest texture ({side} a side) even at its coarsest: the parts show unpainted");
                (super::paint::PaintPack { data: vec![0.0; table], elements: Vec::new(), sdf: Vec::new(), coarser: String::new() }, table_rows)
            }
        };
        if pack.coarser.is_empty() && !self.coarser.is_empty() {
            log!("fab viewport: the paint atlas fits the device's largest texture again: drawn at its own cells");
        }
        self.coarser = pack.coarser.clone();
        let realloc = self.texture.is_none() || self.width != width || self.height != height;
        let mut data = pack.data;
        self.painted = pack.elements;
        self.write_table(&mut data[..table], state, pane, sdf);
        if realloc {
            self.texture = Some(Texture::new_with_format(
                cx,
                TextureFormat::VecRGBAf32 {
                    width,
                    height,
                    data: Some(data),
                    updated: TextureUpdated::Full,
                },
            ));
        } else {
            let t = self.texture.as_ref().unwrap();
            if !same {
                // (a coarser atlas of the texture's size: its data was not taken)
                drop(t.take_vec_f32(cx));
            }
            t.put_back_vec_f32(cx, data, None);
        }
        self.width = width;
        self.height = height;
        self.table_rows = table_rows;
        self.atlas = Some(atlas);
        self.built = Some(key);
        paint.bind(self.texture.clone(), width as f32, height as f32);
        LutSync::All
    }

    /// The element table (`table`: its rows of the texture's data), with each painted element's
    /// header of the atlas packed last.
    fn write_table(&mut self, table: &mut [f32], state: &AppState, pane: PaneId, sdf: &[ElementId]) {
        self.shown_bounds = fill(table, state, pane, sdf);
        self.casters = casters_key(table, state.scene.generation);
        for (e, h) in &self.painted {
            let at = (e * STRIDE + 1) * 4 + 3;
            if at < table.len() {
                table[at] = *h;
            }
        }
    }
}

fn fill(data: &mut [f32], state: &AppState, pane: PaneId, sdf: &[ElementId]) -> (Aabb, Aabb) {
    let scene = &state.scene;
    let ss = &state.scene_state;
    let n = scene.elements.len();
    let poses = state.pane_poses(pane);
    let caps: Vec<ElementId> = elec_selected_caps(state).collect();
    let (mut shown, mut meshes_moved) = (aabb_empty(), aabb_empty());
    for i in 0..n {
        let id = ElementId::from_index(i);
        let o = i * 4 * STRIDE;
        if o + 4 * STRIDE > data.len() {
            break;
        }
        let visible = state.is_drawn_in(id, pane);
        let st = if !visible {
            STATE_HIDDEN
        } else if ss.selection.active == Some(id) || caps.contains(&id) {
            STATE_ACTIVE
        } else if ss.selection.contains(id) {
            STATE_SELECTED
        } else {
            STATE_VISIBLE
        };
        // A rigid move in Fab's frame (Z up, metres): the pane's set's, else Mechanics', else the
        // placement preview's.
        let pose = state.pose_under(poses, id);
        let (rot, by) = match pose {
            Some(a) => ([[a[0][0], a[0][1], a[0][2]], [a[1][0], a[1][1], a[1][2]], [a[2][0], a[2][1], a[2][2]]], vec3(a[0][3], a[1][3], a[2][3])),
            None => ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], vec3(0.0, 0.0, 0.0)),
        };
        let off = to_render(by);
        let r = rot_to_render(&rot);
        data[o] = if sdf.contains(&id) { -st } else { st };
        data[o + 1] = off.x;
        data[o + 2] = off.y;
        data[o + 3] = off.z;
        let ghost = state.is_ghost_drawn_under(poses, id);
        // The same opaque states used by aux, lit and the cascades determine their bounds.
        // A pose is applied to all eight corners (a rotated box is not just min/max moved).
        let rest = scene.elements[i].bounds;
        if visible && !ghost && !aabb_is_empty(&rest) {
            let bounds = if let Some(pose) = pose {
                let mut bounds = aabb_empty();
                for bits in 0..8 {
                    let corner = vec3(
                        if bits & 1 == 0 { rest.min.x } else { rest.max.x },
                        if bits & 2 == 0 { rest.min.y } else { rest.max.y },
                        if bits & 4 == 0 { rest.min.z } else { rest.max.z },
                    );
                    bounds = crate::model::bounds::aabb_union_point(&bounds, crate::model::state::pose_apply(&pose, corner));
                }
                bounds
            } else { rest };
            shown = aabb_union(&shown, &bounds);
            if pose.is_some() && !sdf.contains(&id) {
                meshes_moved = aabb_union(&meshes_moved, &bounds);
            }
        }
        let ghost_state = if !ghost {
            STATE_HIDDEN
        } else if ss.selection.active == Some(id) {
            STATE_ACTIVE
        } else if ss.selection.contains(id) {
            STATE_SELECTED
        } else {
            STATE_VISIBLE
        };
        let arriving = ghost && state.plate.part_of(id).is_some_and(|k| state.plate.parts[k].arriving);
        let ghost_state = ghost_state + if arriving { ARRIVING } else { 0.0 };
        let colour = state.ghost_color(id).map_or(0.0, ghost_colour);
        for k in 0..3 {
            data[o + 4 * (k + 1)] = r[k][0];
            data[o + 4 * (k + 1) + 1] = r[k][1];
            data[o + 4 * (k + 1) + 2] = r[k][2];
            data[o + 4 * (k + 1) + 3] = 0.0;
        }
        data[o + 4 * 2 + 3] = ghost_state;
        let uncut = state.test_cut_shown() && state.plate.context_elements.contains(&id);
        data[o + 4 * 3 + 3] = if ghost { colour } else if uncut { UNCUT } else { 0.0 };
    }
    // Every texel past the element count reads as hidden, so a stray index
    // never lights up a phantom.
    for t in (n * STRIDE)..(data.len() / 4) {
        data[t * 4] = STATE_HIDDEN;
        data[t * 4 + 1] = 0.0;
        data[t * 4 + 2] = 0.0;
        data[t * 4 + 3] = 0.0;
    }
    (shown, meshes_moved)
}

/// A ghost's colour (linear, as parts carry it) as the lookup carries it: 8-bit sRGB, exact in
/// an f32, as the composite mixes it over what it shows.
fn ghost_colour(c: [f32; 4]) -> f32 {
    let [r, g, b] = crate::ui::popover::display_rgb([c[0], c[1], c[2]]);
    let q = |v: f32| (v * 255.0 + 0.5).floor();
    q(r) * 65536.0 + q(g) * 256.0 + q(b)
}

/// [`ElementLut::casters`] of `data`.
fn casters_key(data: &[f32], generation: u64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    generation.hash(&mut h);
    for (i, e) in data.chunks_exact(4 * STRIDE).enumerate() {
        if e[0] >= 0.5 {
            i.hash(&mut h);
            for (k, v) in e.iter().enumerate() {
                // the state's value is not what a caster reads: only that it draws
                if k != 0 {
                    v.to_bits().hash(&mut h);
                }
            }
        }
    }
    h.finish()
}

/// A rotation of Fab's frame as the same rotation in render space: `C R C^-1` with `C (x, y, z) = (x, z, -y)`.
fn rot_to_render(r: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    // C as a matrix (row-major) and its inverse (its transpose)
    let c = [[1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, -1.0, 0.0f32]];
    let mul = |a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]| -> [[f32; 3]; 3] { std::array::from_fn(|i| std::array::from_fn(|j| (0..3).map(|k| a[i][k] * b[k][j]).sum())) };
    let ct = std::array::from_fn(|i| std::array::from_fn(|j| c[j][i]));
    mul(&mul(&c, r), &ct)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parts() -> Scene {
        Scene::from_model(crate::model::fixture::parts(), &mut |_| {})
    }

    #[test]
    fn lut_marks_hidden_and_selected() {
        let scene = parts();
        let mut state = AppState::default();
        state.scene = std::sync::Arc::new(scene);
        let first = ElementId::from_index(0);
        state.scene_state.set_hidden(first, true);
        let n = state.scene.elements.len() * STRIDE;
        let mut data = vec![0.0f32; LUT_WIDTH * ((n + LUT_WIDTH - 1) / LUT_WIDTH) * 4];
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[0], STATE_HIDDEN);
        state.scene_state.set_hidden(first, false);
        state.scene_state.select_only(first);
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[0], STATE_ACTIVE);
        // a printed keycap bound to a switch the Electronics views selected: ringed as active
        state.scene_state.clear_selection();
        let comp = fab_elec_core::CompRef { board: fab_elec_core::BoardId(0), comp: fab_elec_core::CompId(3) };
        state.part_presses = std::sync::Arc::new([PartPress { element: first, comp }]);
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[0], STATE_VISIBLE, "a keycap whose switch is not selected stays visible");
        let before = view_key(&state, PaneId::PRIMARY);
        state.elec_selected = std::sync::Arc::new([comp]);
        assert!(view_key(&state, PaneId::PRIMARY) != before, "selecting its switch writes the table again");
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[0], STATE_ACTIVE, "a keycap whose switch the Electronics views selected is ringed");
    }

    /// The floor (the bed, `fab:bed`) leaves the table while the camera is under the bed plane,
    /// as a hidden element does, and is back the moment it rises over it; the rings on it stay.
    #[test]
    fn lut_leaves_the_floor_out_from_under_the_bed() {
        let mut state = AppState::default();
        state.scene = std::sync::Arc::new(Scene::from_model(crate::model::fixture::mail_model(4), &mut |_| {}));
        state.sync_plate_elements();
        let id = |name: &str| state.scene.elements.iter().find(|e| e.name == name).expect(name).id;
        let (bed, ring) = (id("fab:bed"), id("mail~0"));
        assert_eq!(state.plate.floor_elements, [bed]);
        let n = state.scene.elements.len() * STRIDE;
        let mut data = vec![0.0f32; LUT_WIDTH * ((n + LUT_WIDTH - 1) / LUT_WIDTH) * 4];
        let at = |e: ElementId| e.index() * 4 * STRIDE;
        for (eye_z, bed_state) in [(0.3, STATE_VISIBLE), (-0.05, STATE_HIDDEN), (0.001, STATE_VISIBLE)] {
            state.view.camera_mut(PaneId::PRIMARY).eye.z = eye_z;
            fill(&mut data, &state, PaneId::PRIMARY, &[]);
            assert_eq!(data[at(bed)], bed_state, "the bed with the eye at {eye_z} m");
            assert_eq!(data[at(ring)], STATE_VISIBLE, "a ring with the eye at {eye_z} m");
        }
    }

    #[test]
    fn lut_carries_ghost_state_and_colour() {
        let scene = parts();
        let mut state = AppState::default();
        state.scene = std::sync::Arc::new(scene);
        let first = ElementId::from_index(0);
        state.plate.parts = vec![PlatePart {
            element: Some(first),
            printed: false,
            color: [1.0, 0.5, 0.0, 1.0],
            ..Default::default()
        }];
        let n = state.scene.elements.len() * STRIDE;
        let mut data = vec![0.0f32; LUT_WIDTH * ((n + LUT_WIDTH - 1) / LUT_WIDTH) * 4];
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[0], STATE_HIDDEN);
        assert_eq!(data[4 * 2 + 3], STATE_VISIBLE);
        // linear 0.5 is sRGB 188
        assert_eq!(data[4 * 3 + 3], (255 * 65536 + 188 * 256) as f32);

        state.scene_state.select_only(first);
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[4 * 2 + 3], STATE_ACTIVE);

        state.plate.parts[0].printed = true;
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[4 * 2 + 3], 0.0);

        // a part on its way in prints, and is a ghost that says so
        state.plate.parts[0].arriving = true;
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        assert_eq!(data[4 * 2 + 3], STATE_ACTIVE + ARRIVING);
    }

    #[test]
    fn lut_carries_a_rigid_move_in_render_space() {
        let scene = parts();
        let mut state = AppState::default();
        state.scene = std::sync::Arc::new(scene);
        let first = ElementId::from_index(0);
        // a quarter turn about Fab's Z (up) and a lift of 1 cm
        let a = [[0.0, -1.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.0, 0.0, 1.0, 0.01f32]];
        let mut m = std::collections::HashMap::new();
        m.insert(first, a);
        state.scene_state.set_moves(Some(std::sync::Arc::new(m)));
        let n = state.scene.elements.len() * STRIDE;
        let mut data = vec![0.0f32; LUT_WIDTH * ((n + LUT_WIDTH - 1) / LUT_WIDTH) * 4];
        fill(&mut data, &state, PaneId::PRIMARY, &[]);
        // Fab Z up is render Y: the offset is (0, 0.01, 0)
        assert_eq!((data[1], data[2], data[3]), (0.0, 0.01, 0.0));
        // about render Y: render x' = x cos - ... : the rows are C R C^-1
        let r0 = [data[4], data[5], data[6]];
        let r1 = [data[8], data[9], data[10]];
        let r2 = [data[12], data[13], data[14]];
        assert_eq!(r1, [0.0, 1.0, 0.0], "render Y stays");
        // Fab x -> Fab y; render (x, z, -y): render x -> render -z
        assert_eq!([r0[0], r1[0], r2[0]], [0.0, 0.0, -1.0]);
        // an element without a move keeps the identity
        assert_eq!([data[16 + 4], data[16 + 9], data[16 + 14]], [1.0, 1.0, 1.0]);
    }
}

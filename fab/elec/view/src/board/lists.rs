//! Drawing board publications: one retained draw list per stream, recorded once and re-recorded
//! only when its publication or a bound texture changes; per frame only the uniforms are set.
//!
//! This is the one entry point for drawing board instance rows, whether from a board's shared
//! drawing (`shared::BoardDraw`) or from any other `BoardBatch` (a proof pane's planes through
//! `scene::scene_of_planes` and `batch::batch_scene`).

use super::batch::KindBatch;
use super::shared::Publication;
use makepad_widgets_core::{makepad_platform::retained_instances::RetainedInstances, *};

/// The textures a board shader samples, by slot (declaration order in `draw.rs`): the host's
/// live net, segment and part rows, the board's item states and the pane's states.
/// Slot 5 is the heat field, sampled by `DrawBoardTri` only, bound only for the substrate list.
#[derive(Clone, Default)]
pub struct BoardTextures {
    pub slots: [Option<Texture>; 6],
}

impl BoardTextures {
    fn ids(&self) -> [Option<TextureId>; 6] {
        std::array::from_fn(|i| self.slots[i].as_ref().map(|t| t.texture_id()))
    }
}

/// The uniforms every board list takes, set per draw item each frame (no re-record).
#[derive(Clone, Copy)]
pub struct BoardUniforms {
    pub cam: [f32; 4],
    pub motion_t: f32,
    pub hover_item: f32,
    pub hover_mix: f32,
    /// Where a hovered pour lights its edge: the pointer, then the radii where the light starts to
    /// fall and where it ends, in the pass's own pixels (`clip`'s); the point far off for none.
    pub hover_at: [f32; 4],
    pub layer_alpha: f32,
    pub layer_ink: [f32; 4],
    pub thin_alpha: f32,
    pub clip: [f32; 4],
    pub live_on: f32,
    pub live_time: f32,
    /// The live ramps' canvas look: 0 dark, 1 light.
    pub live_light: f32,
    pub item_states_on: f32,
    pub pane_states_on: f32,
    /// Whether the substrate samples its heat field, 0 off and 1 on.
    pub heat_on: f32,
    /// The heat field's board-plane box in mm, `[min_x, min_y, max_x, max_y]`.
    pub heat_box: [f32; 4],
    /// How far the heat has faded in, 0..1.
    pub heat_mix: f32,
    /// Items with an index at or past this are a preview's own (not committed yet); -1 for none.
    pub fresh_item: f32,
    /// How far those items lean to the proposal ink, 0..1.
    pub fresh_mix: f32,
    /// 1 while those items are a running route job's partial: they draw faint with a bright edge
    /// (steady, no breath) until the job commits them; 0 otherwise.
    pub fresh_pending: f32,
    /// The board net lit in this pane (instances whose `net` matches), -1 for none.
    pub lit_net: f32,
    /// How strongly the lit net stands out (and the rest steps back), 0..1.
    pub lit_mix: f32,
    /// Non-zero: layer-art coverage into these channels instead of ink (the art raster).
    pub art_mask: [f32; 4],
}

impl Default for BoardUniforms {
    fn default() -> Self {
        Self {
            cam: [0.0, 0.0, 1.0, 1.0],
            motion_t: 0.0,
            hover_item: -1.0,
            hover_mix: 0.0,
            hover_at: [-1e6, -1e6, 1.0, 2.0],
            layer_alpha: 1.0,
            layer_ink: [1.0; 4],
            thin_alpha: 0.4,
            clip: [-1e20, -1e20, 1e20, 1e20],
            live_on: 0.0,
            live_time: 0.0,
            live_light: 0.0,
            item_states_on: 0.0,
            pane_states_on: 0.0,
            heat_on: 0.0,
            heat_box: [0.0, 0.0, 1.0, 1.0],
            heat_mix: 0.0,
            fresh_item: -1.0,
            fresh_mix: 0.0,
            fresh_pending: 0.0,
            lit_net: -1.0,
            lit_mix: 0.0,
            art_mask: [0.0; 4],
        }
    }
}

/// One stream as a pane draws it: its publication and the draw list that records it.
pub struct StreamList {
    publication: RetainedInstances,
    list: Option<DrawList2d>,
    bound: [Option<TextureId>; 6],
    dirty: bool,
}

impl StreamList {
    pub fn new(publication: RetainedInstances) -> StreamList {
        StreamList { publication, list: None, bound: [None; 6], dirty: true }
    }

    /// A batch stream's rows as a list (`None` when it has none or cannot be published).
    pub fn of(kind: &KindBatch) -> Option<StreamList> {
        if kind.is_empty() {
            return None;
        }
        match kind.publication() {
            Ok(p) => Some(StreamList::new(p)),
            Err(e) => {
                log!("fab.board: a stream of {} instances is not drawn: {e}", kind.count);
                None
            }
        }
    }

    /// Keeps `slot` in step with `publication`: the same publication keeps the recorded list, a new
    /// one marks it for re-recording (the frame builder uploads only the segments that changed).
    pub fn sync(slot: &mut Option<StreamList>, publication: &Publication) {
        match (slot.as_mut(), publication) {
            (Some(list), Some(p)) => {
                if list.publication.id() != p.id() {
                    list.publication = p.clone();
                    list.dirty = true;
                }
            }
            (None, Some(p)) => *slot = Some(StreamList::new(p.clone())),
            (_, None) => *slot = None,
        }
    }

    pub fn publication(&self) -> &RetainedInstances {
        &self.publication
    }

    /// Re-record at the next draw (the pane switched the shader it draws this list with).
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Draws the list with `vars`' shader: records it when new, re-published or re-bound, then sets
    /// this frame's uniforms on its draw items. O(draw items x uniforms).
    pub fn draw(&mut self, cx: &mut Cx2d, vars: &mut DrawVars, tex: &BoardTextures, u: &BoardUniforms) {
        self.draw_with(cx, vars, tex, u, &[]);
    }

    /// As `draw`, with uniforms specific to this stream (e.g. the moving footprint underlay).
    pub(super) fn draw_with(&mut self, cx: &mut Cx2d, vars: &mut DrawVars, tex: &BoardTextures, u: &BoardUniforms,
        extra: &[(LiveId, &[f32])])
    {
        let ids = tex.ids();
        let dirty = self.dirty || ids != self.bound;
        let dl = self.list.get_or_insert_with(|| DrawList2d::new(cx));
        if dl.begin_maybe(cx, dirty).is_ok() {
            cx.begin_root_turtle(dvec2(1.0e7, 1.0e7), Layout::flow_overlay());
            vars.options.depth_write = false;
            for (slot, t) in tex.slots.iter().enumerate() {
                match t {
                    Some(t) => vars.set_texture(slot, t),
                    None => vars.empty_texture(slot),
                }
            }
            cx.add_retained_instances(vars, &self.publication);
            cx.end_pass_sized_turtle();
            dl.end(cx);
        }
        self.dirty = false;
        self.bound = ids;
        let id = dl.id();
        let values: [(LiveId, &[f32]); 23] = [
            (live_id!(cam), &u.cam),
            (live_id!(motion_t), &[u.motion_t]),
            (live_id!(hover_item), &[u.hover_item]),
            (live_id!(hover_mix), &[u.hover_mix]),
            (live_id!(hover_at), &u.hover_at),
            (live_id!(layer_alpha), &[u.layer_alpha]),
            (live_id!(layer_ink), &u.layer_ink),
            (live_id!(thin_alpha), &[u.thin_alpha]),
            (live_id!(clip), &u.clip),
            (live_id!(live_on), &[u.live_on]),
            (live_id!(live_time), &[u.live_time]),
            (live_id!(live_light), &[u.live_light]),
            (live_id!(item_states_on), &[u.item_states_on]),
            (live_id!(pane_states_on), &[u.pane_states_on]),
            (live_id!(heat_on), &[u.heat_on]),
            (live_id!(heat_box), &u.heat_box),
            (live_id!(heat_mix), &[u.heat_mix]),
            (live_id!(fresh_item), &[u.fresh_item]),
            (live_id!(fresh_mix), &[u.fresh_mix]),
            (live_id!(fresh_pending), &[u.fresh_pending]),
            (live_id!(lit_net), &[u.lit_net]),
            (live_id!(lit_mix), &[u.lit_mix]),
            (live_id!(art_mask), &u.art_mask),
        ];
        for item in 0..cx.draw_lists[id].draw_items.len() {
            for &(name, value) in values.iter().chain(extra) {
                cx.set_draw_item_uniform(id, item, name, value);
            }
        }
    }
}

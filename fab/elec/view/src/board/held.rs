//! Retained copies of dragged board items: the part at the cursor and a ghost at its committed
//! place, both drawn in the pane's main pass. The pane drops this drawing when the drag ends.
//!
//! `HeldDraw::new` costs O(held items) on the UI thread, measured in `build_ms`. Each copy owns
//! separate lists because a DrawList2d is drawn at most once per frame. After their first draw
//! records them, each frame sets one set of uniforms per list, with no rebuild or re-record.
//! Moving the held copy changes only the camera uniforms; instances stay at their committed place.

use fab_elec_board::{Board, ZoneFill};
use fab_elec_core::{BBox, ItemId, LayerId, P};
use makepad_widgets_core::{Cx, Cx2d, DrawVars};

use super::{
    batch::{batch_scene, BoardBatch, Templates},
    lists::{BoardTextures, BoardUniforms, StreamList},
    liveix::LiveIx,
    scene::scene_of_items,
    theme::BoardTheme,
};
use crate::canvas::Camera2d;

/// How strongly pending copper mixes toward the proposal ink.
pub(super) const PENDING_MIX: f32 = 0.55;

/// How a held copy looks.
#[derive(Clone, Copy, Debug)]
pub struct HeldLook {
    /// Multiplies each layer's opacity.
    pub alpha: f32,
    /// 0 keeps the layer ink, 1 makes it its Rec. 709 luminance grey.
    pub desaturate: f32,
    /// The drop is waiting for its answer, so the copy uses pending proposal ink. Item IDs at
    /// or above `1 << 24` exceed the shader's item range and draw as settled.
    pub pending: bool,
}

pub struct HeldDraw {
    /// The held items, as handed to `new`.
    pub items: Vec<ItemId>,
    /// The held items' scene bounds, nm (world), at their committed place.
    pub bbox: BBox,
    /// Instances in one copy.
    pub instances: usize,
    /// `new`'s wall time, ms (UI thread; O(the held items' size)).
    pub build_ms: f64,
    held: HeldCopy,
    ghost: HeldCopy,
}

/// The pane's direct (non-max) shaders' vars.
pub struct HeldVars<'a> {
    pub stroke: &'a mut DrawVars,
    pub pad: &'a mut DrawVars,
    pub tri: &'a mut DrawVars,
}

struct HeldLayer {
    layer: LayerId,
    tris: Option<StreamList>,
    strokes: Option<StreamList>,
    pads: Option<StreamList>,
}

struct HeldCopy {
    // Stored in stack order so drawing needs neither sorting nor layer lookups.
    layers: Vec<HeldLayer>,
    holes: Option<StreamList>,
}

impl HeldDraw {
    pub fn new(board: &Board, fills: &[ZoneFill], items: &[ItemId], t: &Templates) -> HeldDraw {
        let started = Cx::monotonic_now();
        let mut out = {
            let scene = scene_of_items(board, fills, items);
            let batch = batch_scene(&scene, t, &LiveIx::none());
            HeldDraw {
                items: items.to_vec(),
                bbox: scene.bbox,
                instances: batch.instances(),
                build_ms: 0.0,
                held: HeldCopy::new(&batch),
                ghost: HeldCopy::new(&batch),
            }
        };
        out.build_ms = (Cx::monotonic_now() - started) * 1e3;
        out
    }

    /// The copy at the cursor: shifted by `by` (nm, world).
    pub fn draw_held(
        &mut self, cx: &mut Cx2d, vars: HeldVars<'_>, theme: &BoardTheme, active: LayerId,
        visible: &dyn Fn(LayerId) -> bool, base: &BoardUniforms, cam: &Camera2d, by: P, look: HeldLook,
    ) {
        self.held.draw(cx, vars, theme, active, visible, base, cam, by, look);
    }

    /// The ghost at the committed place (no shift).
    pub fn draw_ghost(
        &mut self, cx: &mut Cx2d, vars: HeldVars<'_>, theme: &BoardTheme, active: LayerId,
        visible: &dyn Fn(LayerId) -> bool, base: &BoardUniforms, cam: &Camera2d, look: HeldLook,
    ) {
        self.ghost.draw(cx, vars, theme, active, visible, base, cam, P::new(0, 0), look);
    }
}

impl HeldCopy {
    fn new(batch: &BoardBatch) -> HeldCopy {
        let layers = BoardTheme::stack(batch.layers.iter().map(|layer| layer.layer))
            .into_iter()
            .filter_map(|layer| batch.layer(layer))
            .map(|layer| HeldLayer {
                layer: layer.layer,
                tris: StreamList::of(&layer.tris),
                strokes: StreamList::of(&layer.strokes),
                pads: StreamList::of(&layer.pads),
            })
            .collect();
        HeldCopy { layers, holes: StreamList::of(&batch.holes) }
    }

    fn draw(
        &mut self, cx: &mut Cx2d, vars: HeldVars<'_>, theme: &BoardTheme, active: LayerId,
        visible: &dyn Fn(LayerId) -> bool, base: &BoardUniforms, cam: &Camera2d, by: P, look: HeldLook,
    ) {
        let mut shifted = *cam;
        shifted.center[0] -= by.x as f64 / 1e6;
        shifted.center[1] -= by.y as f64 / 1e6;
        let mut base = BoardUniforms {
            cam: shifted.uniforms(),
            hover_item: -1.0,
            live_on: 0.0,
            item_states_on: 0.0,
            pane_states_on: 0.0,
            ..*base
        };
        if look.pending {
            base.fresh_item = 0.0;
            base.fresh_mix = PENDING_MIX;
            base.fresh_pending = 1.0;
        }
        let tex = BoardTextures::default();
        for layer in &mut self.layers {
            if !visible(layer.layer) {
                continue;
            }
            let alpha = theme.opacity(layer.layer, active) * look.alpha;
            if alpha < 0.004 {
                continue;
            }
            let mut ink = theme.ink(layer.layer);
            let luminance = 0.2126 * ink[0] + 0.7152 * ink[1] + 0.0722 * ink[2];
            for channel in &mut ink[..3] {
                *channel += (luminance - *channel) * look.desaturate;
            }
            let u = BoardUniforms { layer_alpha: alpha, layer_ink: ink, ..base };
            if let Some(list) = layer.tris.as_mut() {
                list.draw(cx, vars.tri, &tex, &u);
            }
            if let Some(list) = layer.strokes.as_mut() {
                list.draw(cx, vars.stroke, &tex, &u);
            }
            if let Some(list) = layer.pads.as_mut() {
                list.draw(cx, vars.pad, &tex, &u);
            }
        }
        if let Some(holes) = self.holes.as_mut() {
            let u = BoardUniforms { layer_alpha: look.alpha, layer_ink: theme.hole, fresh_item: -1.0, ..base };
            holes.draw(cx, vars.pad, &tex, &u);
        }
    }
}

//! Layer art drawn on the GPU from the board's shared publications.
//!
//! `Surface(side)` stores outer copper in r, mask openings in g, silk in b and drills in a.
//! `Paste(side)` stores paste in r and zero in the other channels. `Net` and `Inner` are
//! reserved and currently clear to zero. These are linear RGBA8 coverage channels (BGRA8
//! storage), not premultiplied colour: drill is data, and nothing is cut or composited here.
//! Each tile includes its apron, with stored row zero at the bottom of the board rectangle.
//!
//! Cost: one pass and up to seven draw items per tile for the usual board streams. The vertex
//! stage draws every instance of each list, so at the 100k scale a tile costs about one frame
//! of vertex work. Rasterising is deterministic: the same publications, tile and camera give
//! the same bits. Max blending unions coverage independently in every channel.

use super::batch::Templates;
use super::draw::{DrawBoardPadMax, DrawBoardStrokeMax, DrawBoardTriMax};
use super::inst::{PadTemplate, StrokeTemplate, TriTemplate};
use super::lists::{BoardTextures, BoardUniforms, StreamList};
use super::shared::{BoardDraw, BoardDraws, LayerPubs, Publication};
use super::tiles::{ArtPlane, BoardArt, TileId, ART_APRON, ART_TILE_STORED};
use fab_elec_core::{BoardId, LayerId};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*

    mod.widgets.BoardArtRaster = set_type_default() do #(ArtRaster::script_api(vm)) {
        draw_stroke: mod.draw.DrawBoardStrokeMax {}
        draw_pad: mod.draw.DrawBoardPadMax {}
        draw_tri: mod.draw.DrawBoardTriMax {}
    }
}

/// A host's `#[live]` draw helper, registered as `mod.widgets.BoardArtRaster`.
#[derive(Script, ScriptHook)]
pub struct ArtRaster {
    #[live]
    draw_stroke: DrawBoardStrokeMax,
    #[live]
    draw_pad: DrawBoardPadMax,
    #[live]
    draw_tri: DrawBoardTriMax,
    #[rust]
    pending: Vec<RasterPass>,
    /// The boards `raster_missing` served in this round (a round ends when one comes again).
    #[rust]
    served: Vec<BoardId>,
    #[rust]
    reserved_logged: bool,
}

/// Keep the recording alive until the GPU has painted it, including pipeline compilation.
/// Afterwards only the caller's texture remains; the lists and pass can be dropped.
struct RasterPass {
    target: RenderTarget,
    lists: Vec<StreamList>,
    repaint: u64,
    /// `raster_missing`'s recordings: the board and tile it replaces once painted, at its board
    /// generation.
    tile: Option<(BoardId, ArtPlane, TileId, u64)>,
}

impl RasterPass {
    fn draw(&mut self, cx: &mut Cx2d, vars: &mut DrawVars, publication: &Publication, u: &BoardUniforms) {
        if let Some(publication) = publication {
            let mut list = StreamList::new(publication.clone());
            list.draw(cx, vars, &BoardTextures::default(), u);
            self.lists.push(list);
        }
    }
}

impl ArtRaster {
    /// Hands the board instance layouts to `BoardDraws` when no board pane has yet (a consumer
    /// that draws art with no pane on screen, such as the 3D view): board drawings wait for them.
    /// Call it each draw until it returns true; afterwards it is one global lookup.
    pub fn ensure_templates(&self, cx: &mut Cx) -> bool {
        if BoardDraws::has_templates(cx) {
            return true;
        }
        match (
            StrokeTemplate::capture(cx, &self.draw_stroke.draw_vars),
            PadTemplate::capture(cx, &self.draw_pad.draw_vars),
            TriTemplate::capture(cx, &self.draw_tri.draw_vars),
        ) {
            (Some(stroke), Some(pad), Some(tri)) => {
                BoardDraws::set_templates(cx, Templates { stroke, pad, tri });
                true
            }
            _ => false,
        }
    }

    /// Records one tile into a new, cleared, single-sample target, apron included. The returned
    /// texture keeps its pixels until dropped. Keep this helper alive until the GPU paints it;
    /// subsequent raster calls retire completed recordings without drawing them again.
    pub fn raster(&mut self, cx: &mut Cx2d, draw: &BoardDraw, plane: ArtPlane, tile: TileId) -> Texture {
        self.finish_pending(cx, None);
        self.record(cx, draw, plane, tile)
    }

    /// Rasterises up to `max_tiles` missing tiles, coarse first, with `draw.gen` as their board
    /// generation. This is a frame budget, not a limit: the rest stay missing for the next frame,
    /// while `BoardArt::cover()` keeps the view filled with resident coarser or older tiles.
    /// Call every draw, even when no tiles are missing, to retire completed GPU recordings.
    ///
    /// A recording replaces its tile only once the GPU has painted it: until then the view keeps
    /// the tile's older texels (or a coarser tile), so a re-rastered tile never shows cleared for
    /// a frame. A tile still in flight is not recorded again; it stays missing until it lands.
    /// Several boards share one rasteriser: each call lands only its own board's tiles, and a
    /// painted tile whose board went unserved for a whole round (the board left) is dropped.
    pub fn raster_missing(&mut self, cx: &mut Cx2d, draw: &BoardDraw, art: &mut BoardArt<Texture>, max_tiles: usize) -> usize {
        if self.served.contains(&draw.unit) {
            let served = std::mem::take(&mut self.served);
            self.pending.retain_mut(|pending| {
                let gone = pending.tile.is_some_and(|(unit, ..)| !served.contains(&unit));
                if gone && pending.target.pass.painted_whole_since(cx, pending.repaint) {
                    pending.target.pass.withdraw(cx);
                    pending.target.pass.clear_color_textures(cx);
                    return false;
                }
                true
            });
        }
        self.served.push(draw.unit);
        self.finish_pending(cx, Some((draw.unit, &mut *art)));
        let flying: Vec<(ArtPlane, TileId)> = self.pending.iter()
            .filter_map(|p| p.tile.filter(|t| t.0 == draw.unit).map(|(_, plane, tile, _)| (plane, tile)))
            .collect();
        let mut count = 0;
        for (plane, tile) in art.missing() {
            if count == max_tiles {
                break;
            }
            if flying.contains(&(plane, tile)) {
                continue;
            }
            drop(self.record(cx, draw, plane, tile));
            if let Some(pass) = self.pending.last_mut() {
                pass.tile = Some((draw.unit, plane, tile, draw.gen));
            }
            count += 1;
        }
        count
    }

    /// Retires painted recordings; with a board's `art`, a painted tile recording of that board
    /// replaces its tile. Another board's painted tile waits for that board's `raster_missing`.
    fn finish_pending(&mut self, cx: &mut Cx2d, mut art: Option<(BoardId, &mut BoardArt<Texture>)>) {
        self.pending.retain_mut(|pending| {
            if pending.target.pass.painted_whole_since(cx, pending.repaint) {
                let landing = match (pending.tile, art.as_mut()) {
                    (None, _) => None,
                    (Some((unit, plane, tile, generation)), Some((mine, art))) if unit == *mine => Some((art, plane, tile, generation)),
                    (Some(_), _) => return true,
                };
                pending.target.pass.withdraw(cx);
                pending.target.pass.clear_color_textures(cx);
                if let Some((art, plane, tile, generation)) = landing {
                    let (_, replaced) = art.put(plane, tile, pending.target.texture.clone(), generation);
                    drop(replaced);
                }
                false
            } else {
                // Reattach the existing recording while a pipeline or its GPU work is pending.
                cx.make_child_pass(&pending.target.pass);
                true
            }
        });
    }

    fn record(&mut self, cx: &mut Cx2d, draw: &BoardDraw, plane: ArtPlane, tile: TileId) -> Texture {
        let mut target = RenderTarget::new(cx, "fab_board_art");
        target.ensure_kept(cx, (ART_TILE_STORED, ART_TILE_STORED), RenderTargetFormat::Bgra8);
        target.texture.set_retained_render_target(cx, true);
        let size = ART_TILE_STORED as f32;
        // The board shaders turn Y up into screen Y down. Reverse the pass's Y projection
        // so stored row zero is the board's bottom, as TileId::uv and texel_center require.
        target.pass.set_keep_camera_matrix(cx, true);
        target.pass.set_camera(cx, Mat4f::identity(), Mat4f::ortho(0.0, size, size, 0.0, 100.0, -100.0, 1.0, 1.0));
        target.begin(cx, vec4(0.0, 0.0, 0.0, 0.0));
        let mut pass = RasterPass { target, lists: Vec::new(), repaint: 0, tile: None };

        let rect = tile.rect_nm();
        let texel = TileId::texel(tile.lod) as f64;
        let apron = ART_APRON as f64 * texel;
        let min_x = (rect.min.x as f64 - apron) / 1e6;
        let max_y = (rect.max.y as f64 + apron) / 1e6;
        let k = 1e6 / texel;
        let base = BoardUniforms {
            cam: [(-min_x * k) as f32, (max_y * k) as f32, k as f32, 1.0],
            layer_alpha: 1.0,
            hover_item: -1.0,
            thin_alpha: 1.0,
            clip: [0.0, 0.0, size, size],
            ..BoardUniforms::default()
        };
        match plane {
            ArtPlane::Surface(side) => {
                for (layer, art_mask) in [
                    (LayerId::F_CU.on(side), [1.0, 0.0, 0.0, 0.0]),
                    (LayerId::F_MASK.on(side), [0.0, 1.0, 0.0, 0.0]),
                    (LayerId::F_SILK.on(side), [0.0, 0.0, 1.0, 0.0]),
                ] {
                    if let Some(layer) = draw.pubs.layers.iter().find(|p| p.layer == layer) {
                        self.layer(cx, &mut pass, layer, &BoardUniforms { art_mask, ..base });
                    }
                }
                pass.draw(cx, &mut self.draw_pad.draw_vars, &draw.pubs.holes, &BoardUniforms { art_mask: [0.0, 0.0, 0.0, 1.0], ..base });
            }
            ArtPlane::Paste(side) => {
                if let Some(layer) = draw.pubs.layers.iter().find(|p| p.layer == LayerId::F_PASTE.on(side)) {
                    self.layer(cx, &mut pass, layer, &BoardUniforms { art_mask: [1.0, 0.0, 0.0, 0.0], ..base });
                }
            }
            ArtPlane::Net(_) | ArtPlane::Inner(_) => {
                if !self.reserved_logged {
                    log!("fab.board art: Net and Inner planes are reserved; returning cleared tiles");
                    self.reserved_logged = true;
                }
            }
        }
        pass.target.end(cx);
        pass.repaint = cx.repaint_id();
        let texture = pass.target.texture.clone();
        self.pending.push(pass);
        texture
    }

    fn layer(&mut self, cx: &mut Cx2d, pass: &mut RasterPass, layer: &LayerPubs, u: &BoardUniforms) {
        pass.draw(cx, &mut self.draw_tri.draw_vars, &layer.tris, u);
        pass.draw(cx, &mut self.draw_stroke.draw_vars, &layer.strokes, u);
        pass.draw(cx, &mut self.draw_pad.draw_vars, &layer.pads, u);
    }
}

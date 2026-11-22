//! `--art`: the board's layer art, drawn on the GPU from the shared drawing and shown in a corner.
//! The left square is the top surface of the whole board at the base level; the right square is
//! the board's centre at a fine level, so both the coarse and the detail rasters are seen.
//! Channels: copper orange, mask opening gold over copper, silk white, drill black.

use fab_elec_core::{BBox, BoardId, Nm, Side, P};
use fab_elec_view::board::{
    art::ArtRaster, ArtPlane, BoardArt, BoardDraws, LayerArt, TileId,
};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    mod.draw.DrawArtTile = set_type_default() do #(DrawArtTile::script_shader(vm)){
        ..mod.draw.DrawQuad
        tex: texture_2d(float)

        pixel: fn() {
            let uv = mix(self.uv_lo, self.uv_hi, self.pos)
            let a = self.tex.sample(uv)
            var c = vec3(0.06, 0.16, 0.10)
            c = mix(c, vec3(0.80, 0.48, 0.22), a.x)
            c = mix(c, vec3(0.93, 0.78, 0.40), a.x * a.y)
            c = mix(c, vec3(0.95, 0.95, 0.92), a.z)
            c = mix(c, vec3(0.0, 0.0, 0.0), a.w)
            return vec4(c, 1.0)
        }
    }

    mod.widgets.ArtPreviewBase = #(ArtPreview::register_widget(vm))
    mod.widgets.ArtPreview = set_type_default() do mod.widgets.ArtPreviewBase{
        width: 800 height: 400
        draw_bg: mod.draw.DrawColor{color: #x0b0d10}
        draw_tile: mod.draw.DrawArtTile{}
        raster: mod.widgets.BoardArtRaster{}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawArtTile {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    uv_lo: Vec2f,
    #[live]
    uv_hi: Vec2f,
}

#[derive(Script, ScriptHook, Widget)]
pub struct ArtPreview {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_tile: DrawArtTile,
    #[live]
    raster: ArtRaster,
    #[rust]
    unit: Option<BoardId>,
    #[rust]
    art: Option<BoardArt<Texture>>,
    #[rust]
    gen: u64,
    #[rust]
    cover: Vec<LayerArt<Texture>>,
    #[rust]
    rastered: u64,
}

/// Tiles drawn per frame: the rest stay owed to later frames.
const TILES_PER_FRAME: usize = 4;
/// The detail square's side in board nm.
const DETAIL_NM: Nm = 8_000_000;

impl ArtPreview {
    /// Show the art of this board (`None` hides the preview).
    pub fn set_unit(&mut self, cx: &mut Cx, unit: Option<BoardId>) {
        self.unit = unit;
        self.redraw(cx);
    }

    fn panel(&mut self, cx: &mut Cx2d, art: &BoardArt<Texture>, plane: ArtPlane, lod: u8, region: BBox, screen: Rect) {
        art.cover_into(plane, lod, region, &mut self.cover);
        let w = (region.max.x - region.min.x) as f64;
        let h = (region.max.y - region.min.y) as f64;
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let k = (screen.size.x / w).min(screen.size.y / h);
        let to_x = |x: f64| screen.pos.x + (x - region.min.x as f64) * k;
        let to_y = |y: f64| screen.pos.y + (region.max.y as f64 - y) * k;
        for la in &self.cover {
            let r = la.rect_nm;
            let lo = (r.min.x.max(region.min.x) as f64, r.min.y.max(region.min.y) as f64);
            let hi = (r.max.x.min(region.max.x) as f64, r.max.y.min(region.max.y) as f64);
            if hi.0 <= lo.0 || hi.1 <= lo.1 {
                continue;
            }
            // Screen top-left is the board's (min x, max y).
            let uv_lo = la.tile.uv((lo.0, hi.1));
            let uv_hi = la.tile.uv((hi.0, lo.1));
            self.draw_tile.uv_lo = vec2(uv_lo.0 as f32, uv_lo.1 as f32);
            self.draw_tile.uv_hi = vec2(uv_hi.0 as f32, uv_hi.1 as f32);
            self.draw_tile.draw_vars.set_texture(0, &la.texture);
            self.draw_tile.new_draw_call(cx);
            let rect = Rect { pos: dvec2(to_x(lo.0), to_y(hi.1)), size: dvec2((hi.0 - lo.0) * k, (hi.1 - lo.1) * k) };
            self.draw_tile.draw_abs(cx, rect);
        }
    }
}

impl Widget for ArtPreview {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        // A new picture of the board owes new art.
        if let (Event::Signal, Some(unit)) = (event, self.unit) {
            if BoardDraws::current(cx, unit).is_some_and(|d| d.gen != self.gen) {
                self.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        let Some(unit) = self.unit else { return DrawStep::done() };
        let Some(draw) = BoardDraws::current(cx, unit) else { return DrawStep::done() };
        self.draw_bg.draw_abs(cx, rect);
        let board = draw.board.outline_bbox();
        let art = self.art.get_or_insert_with(|| BoardArt::new(board, 256 << 20));
        if draw.gen != self.gen {
            art.set_board(board);
            art.advance(draw.gen, &[board]);
            self.gen = draw.gen;
        }
        let plane = ArtPlane::Surface(Side::Top);
        let base = BoardArt::<Texture>::base_lod(board);
        let c = board.center();
        let h = DETAIL_NM / 2;
        let detail = BBox::new(P::new(c.x - h, c.y - h), P::new(c.x + h, c.y + h));
        // The detail square's texel: about one screen pixel at the preview's size.
        let detail_lod = TileId::lod_for_texel(DETAIL_NM as f64 / (rect.size.y.max(1.0)));
        art.begin_frame();
        art.want_region(plane, base, board);
        art.want_region(plane, detail_lod, detail);
        let mut art = self.art.take().unwrap();
        let n = self.raster.raster_missing(cx, &draw, &mut art, TILES_PER_FRAME);
        drop(art.evict());
        if n > 0 {
            self.rastered += n as u64;
            log!("boardbench art rastered={} (lod {base} base, lod {detail_lod} detail, gen {})", self.rastered, draw.gen);
        }
        let half = rect.size.x * 0.5;
        let left = Rect { pos: rect.pos, size: dvec2(half, rect.size.y) };
        let right = Rect { pos: dvec2(rect.pos.x + half, rect.pos.y), size: dvec2(half, rect.size.y) };
        self.panel(cx, &art, plane, base, board, left);
        self.panel(cx, &art, plane, detail_lod, detail, right);
        let owed = !art.missing().is_empty();
        self.art = Some(art);
        // Keep drawing while tiles are owed, and once more so finished recordings retire.
        if owed || n > 0 {
            self.redraw(cx);
        }
        DrawStep::done()
    }
}

//! Asset-browser tile grid extracted from the donor console.
//!
//! `VjTileGrid` is the ai-content gallery pattern: PortalList rows of fixed
//! card slots. Recycled rows restart from the template with no texture, so
//! every visible pass rebinds thumbnails from the entry list — textures are
//! keyed upstream by immutable revision, never by list position.

use makepad_asset_data::AssetId;
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    // Same as asset-ui SpriteFitImage's rotation-aware, nearest-filtered
    // sampling (crisp Doom/Quake texels) — extended with a shader-only
    // ASPECT-FIT / ASPECT-FILL toggle. The widget's own Walk rect is
    // always full-bleed now (`ImageFit.Stretch`, no CPU aspect resize);
    // FIT (letterbox) vs FILL (cover, centre-cropped) is purely a UV remap
    // against the actual draw rect (`self.rect_size`) and the bound
    // texture's aspect (`img_aspect`). `fit_scale`/`cover_scale` in this
    // file are the pure-Rust equivalent of `fill_uv`'s min/max maths
    // (unit-tested in `tile_fit_tests`).
    let SpriteTileImage = Image{
        width: Fill
        height: Fill
        fit: ImageFit.Stretch
        draw_bg +: {
            // 1.0 = ASPECT-FILL (cover, default for grid/pad tiles): the
            // image fills the rect, cropping the overflow. 0.0 =
            // ASPECT-FIT (letterbox): the whole image stays visible,
            // inset with transparent bars — VjTileGrid/VjPadMatrix force
            // this to 0 whenever an entry has more than one frame (a
            // sprite-sheet / billboard animation), because cropping into
            // a packed sheet would show a neighbour cell.
            fill: instance(1.0)
            // Aspect (w/h) of the currently bound texture; the host
            // pushes this in every time it rebinds the thumbnail.
            img_aspect: instance(1.0)
            // FIT inscribes the image with `min` (the axis that would
            // overflow shrinks below 1, centring the image with bars on
            // the other axis); FILL covers with `max` (the axis that
            // would underflow instead grows past 1, cropping it) — same
            // computation, `mix`ed by `fill`. Returns (u, v, alpha-mask):
            // FIT's mask is 0 outside the inscribed image (the letterbox
            // bar); FILL's mask is always 1 (it always fully covers, so
            // the clamp below never smears real content, just guards
            // float roundoff at the very edge).
            fill_uv: fn(p: vec2) -> vec3 {
                let ra = max(self.rect_size.x, 1.0) / max(self.rect_size.y, 1.0)
                let ia = max(self.img_aspect, 0.0001)
                let ratio = ia / ra
                // Not all the way to cover: full FILL crops a 16:9 clip's
                // sides so hard the subject leaves the tile. `fill` is the
                // BLEND between contain and cover (see TILE_CROP), so the
                // picture reads big without losing its middle.
                let w = mix(min(1.0, ratio), max(1.0, ratio), self.fill)
                let h = mix(min(1.0, 1.0 / ratio), max(1.0, 1.0 / ratio), self.fill)
                let u = vec2(
                    (p.x - (1.0 - w) * 0.5) / w,
                    (p.y - (1.0 - h) * 0.5) / h
                )
                let inside = step(0.0, u.x) * step(u.x, 1.0) * step(0.0, u.y) * step(u.y, 1.0)
                let uc = clamp(u, vec2(0.0, 0.0), vec2(1.0, 1.0))
                return vec3(uc.x, uc.y, inside)
            }
            get_color_scale_pan: fn(scale: vec2, pan: vec2) {
                if self.image_dim_w > 0.0 {
                    let angle = self.rotation * 3.141592653589793 / 180.0
                    let cos_a = cos(-angle)
                    let sin_a = sin(-angle)
                    let c = (self.pos - vec2(0.5, 0.5)) * self.rect_size
                    let cr = vec2(c.x * cos_a - c.y * sin_a, c.x * sin_a + c.y * cos_a)
                    let iuv = cr / vec2(self.image_dim_w, self.image_dim_h) + vec2(0.5, 0.5)
                    let uv = iuv * scale + pan
                    if uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0 {
                        return vec4(0.0, 0.0, 0.0, 0.0)
                    }
                    return self.image_texture.sample_nearest(uv)
                }
                let framed = self.fill_uv(self.pos)
                let uv = vec2(framed.x, framed.y) * scale + pan
                let c = self.image_texture.sample_nearest(uv)
                // Outside the inscribed picture the sample is a clamped
                // edge texel — drawing it read as "stretched edges". The
                // bars are BLACK now: opaque for photo/clip tiles (any
                // fill), transparent for pure-FIT sprites (fill 0) so the
                // pad face still shows around an icon.
                let bar = step(0.0001, self.fill)
                return vec4(c.xyz * framed.z, c.w * mix(framed.z, 1.0, bar))
            }
            // Subtle top/bottom vignette so TileCell's top-row state label
            // and the title (both cells' bottom row) still read over a
            // full-bleed FILL photo. Scaled by `fill` so it never
            // touches FIT sprite icons; TileCell's own label
            // backdrop already covers its bottom row, so
            // this is redundant-but-harmless there and only load-bearing
            // for PadCell, which has no backdrop of its own.
            edge_scrim: fn(y: float) -> float {
                // smoothstep(edge0, edge1, x) is only well-defined for
                // edge0 < edge1, so the top fade is built as an inverted
                // ascending smoothstep rather than a descending one.
                let top = 1.0 - smoothstep(0.0, 0.32, y)
                let bottom = smoothstep(0.62, 1.0, y)
                return max(top, bottom) * 0.45 * self.fill
            }
            // ONE-PASS TILE FRAME (the tile wall's law): with `radius` set
            // the SAME shader that samples the thumbnail also rounds the
            // corners and strokes the outline — inside the rounded rect
            // it is the image, at the rim the border color, outside
            // transparent. Every pixel of a tile is touched exactly ONCE
            // (the card behind skips its fill — see PadCell.has_thumb).
            // radius 0 = the plain full-quad image every other host gets.
            radius: instance(0.0)
            border_size: 1.0
            border_color: theme.flow_edge_soft
            border_color_selected: instance(theme.flow_accent)
            selected: instance(0.0)
            // Shown only while async_load blends the sample away. A loaded
            // thumbnail stays get_color(); this well does not tint it.
            placeholder: theme.flow_surface_deep
            pixel: fn() {
                let color = mix(self.get_color(), self.placeholder, self.async_load)
                let dim = 1.0 - self.edge_scrim(self.pos.y)
                let img = Pal.premul(vec4(color.xyz * dim, color.w * self.opacity))
                if self.radius < 0.5 {
                    return img
                }
                let r = self.radius
                let h = vec2(self.rect_size.x * 0.5, self.rect_size.y * 0.5)
                let q = abs(self.pos * self.rect_size - h) - h + vec2(r + 1.0, r + 1.0)
                let d = length(max(q, vec2(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r
                let bs = self.border_size + self.selected
                let covered = 1.0 - smoothstep(-0.7, 0.7, d)
                let inside = 1.0 - smoothstep(-bs - 0.7, -bs + 0.7, d)
                let rim = max(covered - inside, 0.0)
                let stroke = self.border_color.mix(self.border_color_selected, self.selected)
                return img * inside + Pal.premul(stroke) * rim
            }
        }
    }
    // The tile says "your click landed and I am working on it".
    //
    // A cue is a fetch, a decode and an upload — a second or more for a
    // world — and until this existed the only sign a click had registered
    // was that eventually the picture changed. It sits ON TOP of the
    // thumbnail (last child of the overlay stack), scrims it so the ring
    // reads over any picture, and turns into a still red ring when the load
    // failed. `spin` is fed from the app clock at draw time, like the beat
    // LED's `since`, so nothing here holds animation state.
    let TileBusy = SolidView{
        width: Fill
        height: Fill
        visible: false
        draw_bg +: {
            spin: instance(0.0)
            failed: instance(0.0)
            color_ring: uniform(theme.flow_highlight)
            color_fail: uniform(theme.flow_error)
            pixel: fn() {
                let p = (self.pos - vec2(0.5, 0.5)) * self.rect_size
                let radius = min(self.rect_size.x, self.rect_size.y) * 0.17
                let width = max(radius * 0.34, 1.5)
                let ring = 1.0 - smoothstep(
                    width * 0.5 - 0.75,
                    width * 0.5 + 0.75,
                    abs(length(p) - radius)
                )
                let tau = 6.283185307179586
                // A comet head that fades round the circle reads as motion
                // from one frame alone; a FAILED load lights the whole ring
                // instead, so a stopped spinner can never be mistaken for a
                // slow one. Sweep runs spin-minus-angle so the BRIGHT head
                // LEADS in the direction of rotation and the tail fades out
                // behind it — the other way round it read as spinning
                // backwards.
                let sweep = fract((self.spin - atan2(p.y, p.x)) / tau)
                let comet = (1.0 - sweep) * (1.0 - sweep)
                let mask = ring * mix(comet, 1.0, self.failed)
                let tint = self.color_ring.mix(self.color_fail, self.failed)
                let scrim = 0.42
                return Pal.premul(vec4(
                    vec3(0.0, 0.0, 0.0).mix(tint.rgb, mask),
                    max(mask, scrim)
                ))
            }
        }
    }
    // The tile a carried pad is hanging over.
    //
    // Its own layer, on top of the picture like the busy ring above it,
    // rather than an instance on the tile's own shader: the wall has to say
    // where a release would land whatever the tile underneath is showing,
    // and a whole layer the host shows or hides needs no value pushed into
    // a shader at draw time.
    let TileDrop = SolidView{
        width: Fill
        height: Fill
        visible: false
        draw_bg +: {
            color_ring: uniform(theme.flow_highlight)
            radius: theme.container_corner_radius
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(1.5, 1.5, self.rect_size.x - 3.0, self.rect_size.y - 3.0, self.radius)
                let wash = self.color_ring
                sdf.fill_keep(vec4(wash.x, wash.y, wash.z, 0.12))
                sdf.stroke(self.color_ring, 3.0)
                return sdf.result
            }
        }
    }
    let TileCell = RoundedView{
        width: 164
        height: 104
        padding: 0
        cursor: MouseCursor.Hand
        draw_bg +: {
            color: theme.flow_surface_deep
            border_color: theme.flow_edge
            border_color_selected: theme.flow_accent
            selected: instance(0.0)
            border_size: 1.0
            border_radius: theme.container_corner_radius
            // SINGLE-FILL LAW (the pad wall's): with a thumbnail bound the
            // IMAGE shader draws the whole tile — picture, rounded corners
            // AND outline — in one pass, so the card paints nothing under
            // it and a square picture can never clip through the corners.
            has_thumb: instance(0.0)
            pixel: fn() {
                if self.has_thumb > 0.5 {
                    return vec4(0.0, 0.0, 0.0, 0.0)
                }
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, self.border_radius)
                sdf.fill_keep(self.color)
                let stroke = self.border_color.mix(self.border_color_selected, self.selected)
                sdf.stroke(stroke, self.border_size + self.selected)
                return sdf.result
            }
        }
        View{
            width: Fill
            height: Fill
            flow: Overlay
            View{
                width: Fill
                height: Fill
                // FULL-BLEED: the shader's own rounding replaces the inset.
                align: Align{x: 0.5 y: 0.5}
                grid_thumb := SpriteTileImage{
                    draw_bg +: {
                        // Sdf2d.box draws 2x its radius, so the image corner
                        // matches TileCell's container_corner_radius.
                        radius: theme.container_corner_radius * 2.0
                        border_size: 1.0
                        border_color: theme.flow_edge
                        border_color_selected: theme.flow_accent
                    }
                }
            }
            View{
                width: Fill
                height: Fill
                flow: Down
                View{
                    width: Fill
                    height: Fit
                    flow: Right
                    spacing: 4
                    padding: Inset{left: 6.0 right: 6.0 top: 5.0 bottom: 0.0}
                    align: Align{x: 0.0, y: 0.5}
                    View{width: Fill height: 1}
                    grid_state := Label{
                        text: ""
                        draw_text.color: theme.flow_highlight
                        draw_text.text_style.font_size: 8
                    }
                }
                View{width: Fill height: Fill}
                SolidView{
                    width: Fill
                    height: Fit
                    flow: Down
                    padding: Inset{left: 6.0 right: 6.0 top: 8.0 bottom: 5.0}
                    draw_bg +: {
                        color: theme.flow_scrim
                        // The label backdrop follows the card's bottom
                        // radius — a square band over rounded corners is
                        // exactly the mismatch the tile law bans.
                        bottom_radius: theme.container_corner_radius
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box_y(1.0, 0.0, self.rect_size.x - 2.0, self.rect_size.y - 1.0, 0.0, self.bottom_radius)
                            sdf.fill(self.color)
                            return sdf.result
                        }
                    }
                    grid_title := Label{
                        width: Fill
                        text: ""
                        draw_text.color: theme.flow_text
                        draw_text.text_style.font_size: 9
                    }
                    grid_sub := Label{
                        width: Fill
                        text: ""
                        draw_text.color: theme.flow_text_body
                        draw_text.text_style.font_size: 8
                    }
                }
            }
            grid_busy := TileBusy{}
            grid_drop := TileDrop{}
        }
    }

    mod.widgets.VjTileGridBase = #(VjTileGrid::register_widget(vm))
    mod.widgets.VjTileGrid = set_type_default() do mod.widgets.VjTileGridBase{
        width: Fill
        height: Fill
        list := PortalList{
            width: Fill
            height: Fill
            flow: Down
            spacing: 4
            // VJ law: a drag belongs to a control, never to a view.
            drag_scrolling: false
            Row := View{
                width: Fill
                height: Fit
                flow: Right
                spacing: 4
                c1 := TileCell{}
                c2 := TileCell{}
                c3 := TileCell{}
                c4 := TileCell{}
                c5 := TileCell{}
                c6 := TileCell{}
                c7 := TileCell{}
                c8 := TileCell{}
            }
            Empty := View{
                width: Fill
                height: 60
                align: Align{x: 0.5, y: 0.5}
                empty_label := Label{
                    text: "no results"
                    draw_text.color: theme.flow_text_muted
                }
            }
        }
    }
    let PadCell = RoundedView{
        width: Fill
        height: Fill
        padding: 0
        cursor: MouseCursor.Hand
        draw_bg +: {
            color: stage.cell
            border_color: stage.border_hi
            border_color_selected: stage.accent
            selected: instance(0.0)
            empty: instance(0.0)
            border_size: 1.0
            border_radius: 5.0
            has_thumb: instance(0.0)
            // One state, one mark: the tile last clicked wears a green
            // ring, every other tile is a plain outline. Anything else the
            // grid used to paint (LIVE / CUE, dim second-place rings) read
            // as noise across forty pads.
            // SINGLE-FILL LAW: with a thumbnail bound, the IMAGE shader
            // draws the whole tile (picture + corners + outline) in one
            // pass — this card paints NOTHING under it, so no pixel on
            // the tile wall fills twice.
            pixel: fn() {
                if self.has_thumb > 0.5 {
                    return vec4(0.0, 0.0, 0.0, 0.0)
                }
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(1.0, 1.0, self.rect_size.x - 2.0, self.rect_size.y - 2.0, self.border_radius)
                // Empty pads fade to a faint outline.
                sdf.fill_keep(self.color.mix(vec4(0.0, 0.0, 0.0, 0.0), self.empty * 0.85))
                sdf.stroke(
                    self.border_color.mix(self.border_color_selected, self.selected)
                        * (1.0 - self.empty * 0.6),
                    self.border_size + self.selected * 1.5
                )
                return sdf.result
            }
        }
        View{
            width: Fill
            height: Fill
            flow: Overlay
            View{
                width: Fill
                height: Fill
                // FULL-BLEED: the thumb shader draws the whole tile in one
                // pass — picture, rounded corners AND outline (radius +
                // border instances below); the card skips its fill under
                // it (has_thumb). The shader's own 1px inset aligns its
                // frame with the card's.
                align: Align{x: 0.5 y: 0.5}
                grid_thumb := SpriteTileImage{
                    draw_bg +: {
                        radius: 10.0
                        border_size: 1.0
                        border_color: stage.border_hi
                        border_color_selected: stage.accent
                    }
                }
            }
            View{
                width: Fill
                height: Fill
                flow: Down
                padding: 3
                View{width: Fill height: Fill}
                // Small name so a pad is readable before its thumbnail lands.
                grid_title := Label{
                    width: Fill
                    flow: Flow.Right{wrap: false}
                    max_lines: 1
                    text: ""
                    draw_text.color: stage.glass_label
                    draw_text.text_style.font_size: 7
                }
                // Where the clip came from ("node 42 · model"), or a pending
                // clip's progress. Fixed glass ink: it sits on the picture.
                grid_sub := Label{
                    visible: false
                    width: Fill
                    flow: Flow.Right{wrap: false}
                    max_lines: 1
                    text: ""
                    draw_text.color: stage.glass_ink_soft
                    draw_text.text_style.font_size: 6
                }
            }
            grid_busy := TileBusy{}
        }
    }


    mod.widgets.VjPadMatrixBase = #(VjPadMatrix::register_widget(vm))
    mod.widgets.VjPadMatrix = set_type_default() do mod.widgets.VjPadMatrixBase{
        width: Fill
        height: Fill
        flow: Down
        spacing: 4
        View{
            width: Fill
            height: Fill
            flow: Right
            spacing: 4
            View{
                width: Fill
                height: Fill
                flow: Down
                spacing: 4
                r0 := View{
                    width: Fill height: Fill flow: Right spacing: 4
                    c1 := PadCell{} c2 := PadCell{} c3 := PadCell{} c4 := PadCell{}
                    c5 := PadCell{} c6 := PadCell{} c7 := PadCell{} c8 := PadCell{}
                }
                r1 := View{
                    width: Fill height: Fill flow: Right spacing: 4
                    c1 := PadCell{} c2 := PadCell{} c3 := PadCell{} c4 := PadCell{}
                    c5 := PadCell{} c6 := PadCell{} c7 := PadCell{} c8 := PadCell{}
                }
                r2 := View{
                    width: Fill height: Fill flow: Right spacing: 4
                    c1 := PadCell{} c2 := PadCell{} c3 := PadCell{} c4 := PadCell{}
                    c5 := PadCell{} c6 := PadCell{} c7 := PadCell{} c8 := PadCell{}
                }
                r3 := View{
                    width: Fill height: Fill flow: Right spacing: 4
                    c1 := PadCell{} c2 := PadCell{} c3 := PadCell{} c4 := PadCell{}
                    c5 := PadCell{} c6 := PadCell{} c7 := PadCell{} c8 := PadCell{}
                }
                r4 := View{
                    width: Fill height: Fill flow: Right spacing: 4
                    c1 := PadCell{} c2 := PadCell{} c3 := PadCell{} c4 := PadCell{}
                    c5 := PadCell{} c6 := PadCell{} c7 := PadCell{} c8 := PadCell{}
                }
            }
        }
        // The strip's horizontal scrollbar. A painted view: an empty View
        // has no area, so the thumb would never find its track.
        // Layout slot only: it must NOT carry `cursor`, or the View
        // hit-tests and swallows the press before the track's own area
        // (drawn over it) ever sees a FingerDown — that is why the thumb
        // could not be grabbed.
        scroll_slot := View{
            width: Fill
            height: 10
            margin: Inset{top: 4}
        }
        draw_track +: {
            color: stage.track
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 3.0)
                sdf.fill(self.color)
                return sdf.result
            }
        }
        draw_thumb +: {
            color: stage.accent
            color_over: uniform(stage.thumb_hover)
            color_press: uniform(stage.text_max)
            hover: instance(0.0)
            down: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(0.0, 0.0, self.rect_size.x, self.rect_size.y, 3.0)
                sdf.fill(self.color.mix(self.color_over, self.hover * 0.5).mix(self.color_press, self.down * 0.4))
                return sdf.result
            }
        }
    }
}

// ---------------------------------------------------------------------------
// tile grid
// ---------------------------------------------------------------------------

pub const GRID_SLOTS: usize = 8;
const CARD_W: f64 = 164.0;
const CARD_SPACING: f64 = 8.0;

#[derive(Clone)]
pub struct GridEntry {
    pub asset: AssetId,
    pub title: String,
    pub sub: String,
    pub state: String,
    pub texture: Option<Texture>,
    pub frames: Vec<Texture>,
    pub fps: f32,
    /// The thumbnail DECLARED a cell layout, so what this tile shows is one
    /// of those cells: a sprite/mesh preview tile its producer already
    /// aspect-fit and centred, padding and all. Such a tile is shown WHOLE
    /// (see [`thumb_fill`]) — cropping into it eats the sprite, and for a
    /// single-frame actor there is no second frame to give the game away.
    /// Straight off the manifest, never measured.
    pub cells: bool,
    /// This tile's cue is being prepared right now — fetching, decoding,
    /// uploading. It wears the spinner until the media is ready (an ARMED
    /// cue is ready: it is only waiting for its beat).
    pub loading: bool,
    /// This tile's load failed; it wears a still red ring until the next
    /// click.
    pub failed: bool,
    /// The one marked tile: on the clip grid the last one CLICKED (green
    /// ring, nothing else), on the SFX bank a pad with voices playing.
    pub active: bool,
    /// The badge wears the LIVE prefix: the tile is on the program (or a
    /// pad is sounding). Separate from `active` because a ring does not
    /// always mean that — the archive explorer rings a SELECTION, and
    /// calling a swatch LIVE would lie about what the room is seeing.
    pub live: bool,
    /// A reserved-but-empty cell of the PENDING head column. It draws as
    /// the quiet grey placeholder and cannot be clicked — the column keeps
    /// its full height from the moment it opens so that filling it never
    /// moves a tile the operator is already reaching for.
    pub placeholder: bool,
    /// A PREFAB cell: a thing the app KNOWS is in the library (a compiled-in
    /// effect preset) whose catalog row has not come back yet. It draws in
    /// full — number, title, the same procedural art the store holds — so
    /// the grid is complete in the first frame and the real row replaces it
    /// in place, with no pop-in and no hole. It just cannot be CLICKED yet:
    /// there is no asset id to fire.
    pub pending: bool,
    /// An EFFECT tile. Its thumbnail walks three phases (prefab art, the
    /// store's placeholder, the baked animated sheet) whose textures may
    /// carry different pixel dims — old stores hold square placeholders —
    /// so the PAINT is what holds the geometry still: an effect tile
    /// always draws full-bleed cover at the tile's own aspect, and a phase
    /// swap can never change the picture's size or framing.
    pub fx: bool,
}

/// A tile whose spinner is turning: it moves between page turns, so a wall
/// showing one is redrawn at [`SPIN_SECS`] whatever its sheets are doing.
fn entry_spins(entry: &GridEntry) -> bool {
    entry.loading || entry_is_baking(entry)
}

/// Effect sheets declare cells even when wasm deliberately captures one
/// representative frame. Frame count therefore cannot double as bake state.
fn entry_is_baking(entry: &GridEntry) -> bool {
    entry.state == "FX" && !entry.cells
}

/// A tile's spinner, written straight into its shader from the wall's draw:
/// the pacer sets how often it turns, and an apply there would redraw it on
/// top of that, at the display's rate.
fn set_busy(cx: &Cx, busy: &ViewRef, now: f64, failed: bool) {
    if let Some(mut busy) = busy.borrow_mut() {
        busy.draw_bg.set_dyn_instance(cx, live_id!(spin), &[busy_spin(now)]);
        busy.draw_bg.set_dyn_instance(cx, live_id!(failed), &[f32::from(u8::from(failed))]);
    }
}

/// Spinner phase (radians) at `time`, ~0.45 turns a second.
fn busy_spin(time: f64) -> f32 {
    const TAU: f64 = std::f64::consts::TAU;
    (time * 2.8).rem_euclid(TAU) as f32
}

/// `pad` staggers a tile's play position by a HASH of its grid slot: forty
/// one-second sheets on one clock pulse like a metronome, and a diagonal
/// wave is still one readable rhythm pulling the eye — decorrelated random
/// phases make the wall shimmer with no pattern to follow at all.
///
/// The stagger is a whole number of FRAMES, never a fraction of one. Every
/// sheet of one rate then turns its page on the same instants, so a wall of
/// forty tiles changes picture thirty times a second rather than on every
/// display frame — which is what lets [`WallPacer`] redraw it at the
/// sheets' rate and not the display's.
fn entry_frame(entry: &GridEntry, time: f64, pad: usize) -> Option<Texture> {
    let len = entry.frames.len();
    if len > 1 {
        let hash = (pad as u32).wrapping_mul(2654435761) >> 8;
        let stagger = (hash & 0xffff) as usize * len / 65536;
        let fps = entry.fps.max(1.0) as f64;
        let i = ((time * fps).floor().max(0.0) as usize + stagger) % len;
        return Some(entry.frames[i].clone());
    }
    entry.texture.clone()
}

/// The moment after `time` at which `entry` next shows a different frame,
/// or `None` for a still.
fn entry_next_flip(entry: &GridEntry, time: f64) -> Option<f64> {
    if entry.frames.len() < 2 {
        return None;
    }
    let fps = entry.fps.max(1.0) as f64;
    Some(((time * fps).floor() + 1.0) / fps)
}

/// What a wall of tiles needs from the clock, gathered over the tiles it
/// actually PAINTED this pass.
#[derive(Clone, Copy, Default)]
struct WallDemand {
    /// The earliest moment any painted sheet turns its page.
    next_flip: Option<f64>,
    /// A painted tile is showing a spinner.
    spinning: bool,
}

impl WallDemand {
    fn note(&mut self, entry: &GridEntry, time: f64) {
        self.spinning |= entry_spins(entry);
        if let Some(at) = entry_next_flip(entry, time) {
            self.next_flip = Some(self.next_flip.map_or(at, |best| best.min(at)));
        }
    }
}

/// Redraws a wall of tiles when its picture changes, and not otherwise.
///
/// A wall used to ask for a frame on every display refresh for as long as
/// any entry in its LIST had a sheet: 120 redraws a second of forty tiles
/// (and of the page around them) to show sheets that turn thirty times a
/// second, going on after the wall had left the screen, because the chain
/// re-armed itself from the event it fired. This arms from the DRAW only,
/// so a wall nobody is drawing asks for nothing, and it sleeps on a timer
/// to the next page turn -- or to the spinner's next step, a ring that
/// turns under half a revolution a second and reads the same at thirty
/// steps a second as at a hundred and twenty.
#[derive(Default)]
struct WallPacer {
    timer: Timer,
}

/// How often a wall with a spinner on it is redrawn.
const SPIN_SECS: f64 = 1.0 / 30.0;

impl WallPacer {
    fn fired(&self, event: &Event) -> bool {
        self.timer.is_event(event).is_some()
    }

    fn arm(&mut self, cx: &mut Cx, time: f64, demand: WallDemand) {
        cx.stop_timer(self.timer);
        self.timer = Timer::default();
        let spin = demand.spinning.then_some(time + SPIN_SECS);
        let wake = match (demand.next_flip, spin) {
            (Some(flip), Some(spin)) => Some(flip.min(spin)),
            (flip, spin) => flip.or(spin),
        };
        if let Some(at) = wake {
            self.timer = cx.start_timeout((at - time).max(0.001));
        }
    }
}

/// `img_aspect / rect_aspect`, guarding non-finite or non-positive inputs
/// (an unloaded texture, a zero-size rect mid-layout) by falling back to an
/// aspect of 1 for whichever side is degenerate — never NaN/inf out.
fn safe_aspect_ratio(rect_aspect: f32, img_aspect: f32) -> f32 {
    let ra = if rect_aspect.is_finite() && rect_aspect > 0.0 { rect_aspect } else { 1.0 };
    let ia = if img_aspect.is_finite() && img_aspect > 0.0 { img_aspect } else { 1.0 };
    ia / ra
}

/// Scale factors `(w, h)` applied to centred UVs to inscribe an image of
/// aspect `img_aspect` inside a rect of aspect `rect_aspect` — ASPECT-FIT /
/// letterbox: the axis that would overflow the rect shrinks below 1, so the
/// whole image stays visible and the other axis is bordered by transparent
/// bars. The pure-Rust mirror of the `fill_uv` shader function's FIT branch
/// (`fill == 0.0`) on `SpriteTileImage` above; kept for the unit tests
/// below (the runtime crop happens on the GPU, from `self.rect_size`).
#[allow(dead_code)]
fn fit_scale(rect_aspect: f32, img_aspect: f32) -> (f32, f32) {
    let ratio = safe_aspect_ratio(rect_aspect, img_aspect);
    (ratio.min(1.0), (1.0 / ratio).min(1.0))
}

/// Scale factors `(w, h)` for ASPECT-FILL / cover: the same computation as
/// [`fit_scale`] with `max` in place of `min` — the axis that would
/// *underflow* the rect grows past 1 instead of the other axis shrinking
/// below it, so the image fully covers the rect and the grown axis crops
/// its overflow. A wide image in a narrower rect crops left/right (`w`
/// grows); a tall image in a wider rect crops top/bottom (`h` grows).
/// Mirrors `fill_uv`'s FILL branch (`fill == 1.0`).
#[allow(dead_code)]
fn cover_scale(rect_aspect: f32, img_aspect: f32) -> (f32, f32) {
    let ratio = safe_aspect_ratio(rect_aspect, img_aspect);
    (ratio.max(1.0), (1.0 / ratio).max(1.0))
}

/// Aspect ratio (`width / height`) of a bound texture, defaulting to 1.0
/// when unknown (not yet uploaded, zero-sized) — matches `fill_uv`'s own
/// `max(self.img_aspect, 0.0001)` guard, so an unknown aspect never NaNs
/// the crop, just treats the tile as square until a real size lands.
fn thumb_aspect(cx: &mut Cx, frame: Option<&Texture>) -> f32 {
    frame
        .and_then(|tex| {
            let format = tex.get_format(cx);
            format.vec_width_height().or_else(|| format.render_fixed_width_height())
        })
        .filter(|(w, h)| *w > 0 && *h > 0)
        .map(|(w, h)| w as f32 / h as f32)
        .unwrap_or(1.0)
}

/// ASPECT-FILL (full-bleed cover) is the default for every tile; only a
/// sprite-sheet / billboard animation (more than one frame swapped over
/// time — see `entry_frame`) stays ASPECT-FIT, because cropping into a
/// packed sheet's cell grid would show a neighbour frame.
/// How far a tile crops toward COVER. 0 is contain (the whole picture with
/// letterbox bars), 1 is full cover (edge to edge, whatever falls outside
/// is gone). Full cover eats too much of a 16:9 clip's sides in a 56px pad,
/// so tiles sit most of the way there: the picture reads large and keeps
/// its middle. One constant — dial it here.
const TILE_CROP: f32 = 0.6;

fn thumb_fill(entry: &GridEntry) -> f32 {
    if entry.fx {
        // Effect tiles are geometry-stable BY LAW (see `GridEntry::fx`):
        // full cover, every phase, whatever texture is bound. The real
        // texture aspect still feeds `img_aspect`, so a square placeholder
        // centre-crops to the tile instead of stretching.
        return 1.0;
    }
    fill_for_thumb(entry.frames.len(), entry.cells)
}

/// How far a tile showing `frames` frames of a `cells`-declared (or not)
/// thumbnail crops toward cover.
///
/// A packed sprite sheet is a GRID of frames: crop it and the cell grid
/// shows a neighbour's arm. Sheets stay contain, always — including the
/// ONE-cell sheet a single-frame actor publishes (`GridEntry::cells`), whose
/// single tile is a whole sprite the producer already letterboxed into a
/// square. Cover-cropping that square in a 164x104 tile would take a quarter
/// of the sprite's height off the top and bottom.
fn fill_for_thumb(frames: usize, cells: bool) -> f32 {
    if frames > 1 || cells {
        0.0
    } else {
        TILE_CROP
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct VjTileGrid {
    #[deref]
    view: View,
    #[rust]
    entries: Vec<GridEntry>,
    #[rust(1usize)]
    pub last_cols: usize,
    #[rust]
    pacer: WallPacer,
    /// The tile a carry is hanging over, which wears a pale ring until the
    /// hand lets go.
    #[rust]
    drop_target: Option<AssetId>,
    /// Where every tile was drawn last pass, as `(index, area)`. A carry
    /// travels outside the tile it is captured by, so what it is over can
    /// only be read off the geometry.
    ///
    /// AREAS, not rects: a rect read while the pass is still drawing is in
    /// whatever space the enclosing scroll has reached so far, and a rect
    /// read off an area once the pass is over is the window's own. The hit
    /// tests the pointer goes through read them the second way, and so must
    /// this.
    #[rust]
    cell_areas: Vec<(usize, Area)>,
}

impl VjTileGrid {
    pub fn set_entries(&mut self, cx: &mut Cx, entries: Vec<GridEntry>) {
        self.entries = entries;
        self.view.redraw(cx);
    }

    /// Mark which tile is loading and which one failed, from the cue engine
    /// (or the pad loader) — pushed every pump rather than waiting for the
    /// next grid rebuild, because a click has to answer NOW and a rebuild
    /// only happens when the catalog changes.
    pub fn set_busy(&mut self, cx: &mut Cx, loading: Option<AssetId>, failed: Option<AssetId>) {
        let mut changed = false;
        for entry in &mut self.entries {
            let is_loading = loading == Some(entry.asset);
            let has_failed = failed == Some(entry.asset);
            if entry.loading != is_loading || entry.failed != has_failed {
                entry.loading = is_loading;
                entry.failed = has_failed;
                changed = true;
            }
        }
        if changed {
            self.view.redraw(cx);
        }
    }

    pub fn set_thumb(&mut self, cx: &mut Cx, asset: AssetId, texture: Texture) {
        self.set_thumb_anim(cx, asset, vec![texture], 0.0);
    }

    pub fn set_thumb_anim(&mut self, cx: &mut Cx, asset: AssetId, frames: Vec<Texture>, fps: f32) {
        let mut hit = false;
        for entry in &mut self.entries {
            if entry.asset == asset {
                entry.texture = frames.first().cloned();
                entry.frames = frames.clone();
                entry.fps = fps;
                hit = true;
            }
        }
        if hit {
            self.view.redraw(cx);
        }
    }

    pub fn has_anims(&self) -> bool {
        self.entries.iter().any(|e| e.frames.len() > 1)
    }

    pub fn entry_at(&self, index: usize) -> Option<&GridEntry> {
        self.entries.get(index)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn set_drop_target(&mut self, cx: &mut Cx, asset: Option<AssetId>) {
        if self.drop_target == asset {
            return;
        }
        self.drop_target = asset;
        self.view.redraw(cx);
    }

    /// The tile under a point, from where the wall last drew itself.
    ///
    /// Inside the grid's own area only: a tile half-scrolled out of the
    /// viewport keeps the whole rect it was clipped from, and a release
    /// over the panel below it is a release over nothing.
    pub fn asset_at(&self, cx: &Cx, abs: DVec2) -> Option<AssetId> {
        if !self.view.area().rect(cx).contains(abs) {
            return None;
        }
        let index = self
            .cell_areas
            .iter()
            .find(|(_, area)| area.rect(cx).contains(abs))
            .map(|(index, _)| *index)?;
        self.entries.get(index).map(|entry| entry.asset)
    }
}

impl Widget for VjTileGrid {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if self.pacer.fired(event) {
            self.view.redraw(cx);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let now = cx.seconds_since_app_start();
        let mut demand = WallDemand::default();
        // The width this grid is about to be laid out at, PEEKED from the
        // turtle. Reading it back from last frame's area does not work on a
        // page that redraws in the same pass (the rect list is reset before
        // this runs, so the area reads empty and the column count never
        // left one — the archive page drew a single column of tiles); the
        // area stays as the fallback for a host that walks with no width.
        let width = {
            let peeked = cx.peek_walk_turtle(walk).size.x;
            if peeked > CARD_W {
                peeked
            } else {
                self.view.area().rect(cx).size.x
            }
        };
        if width > CARD_W {
            self.last_cols = (((width + CARD_SPACING) / (CARD_W + CARD_SPACING)) as usize)
                .clamp(1, GRID_SLOTS);
        }
        let cols = self.last_cols.max(1);
        self.cell_areas.clear();
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = step.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else { continue };
            if self.entries.is_empty() {
                list.set_item_range(cx, 0, 1);
                while let Some(row_id) = list.next_visible_item(cx) {
                    if row_id >= 1 {
                        continue;
                    }
                    let item = list.item(cx, row_id, id!(Empty));
                    item.draw_all(cx, &mut Scope::empty());
                }
                continue;
            }
            let rows = self.entries.len().div_ceil(cols);
            list.set_item_range(cx, 0, rows);
            let slots = [
                ids!(c1),
                ids!(c2),
                ids!(c3),
                ids!(c4),
                ids!(c5),
                ids!(c6),
                ids!(c7),
                ids!(c8),
            ];
            while let Some(row_id) = list.next_visible_item(cx) {
                // PortalList deliberately probes past a short item range to
                // fill the viewport. Instantiating one of those synthetic
                // rows gives it only hidden, zero-height cells, so it asks
                // for another forever and grows the widget tree without
                // bound. Only real catalog rows may create widgets.
                if row_id >= rows {
                    continue;
                }
                let item = list.item(cx, row_id, id!(Row));
                for (slot, path) in slots.iter().enumerate() {
                    let index = row_id * cols + slot;
                    let visible = slot < cols && index < self.entries.len();
                    item.view(cx, *path).set_visible(cx, visible);
                    if !visible {
                        continue;
                    }
                    let entry = &self.entries[index];
                    let cell = item.view(cx, *path);
                    cell.label(cx, ids!(grid_title)).set_text(cx, &entry.title);
                    cell.label(cx, ids!(grid_sub)).set_text(cx, &entry.sub);
                    let state = if entry.live {
                        format!("LIVE {}", entry.state)
                    } else {
                        entry.state.clone()
                    };
                    cell.label(cx, ids!(grid_state)).set_text(cx, &state);
                    // Recycled rows lose their texture: rebind every pass.
                    demand.note(entry, now);
                    let frame = entry_frame(entry, now, index);
                    let aspect = thumb_aspect(cx, frame.as_ref());
                    let fill = thumb_fill(entry);
                    // No thumbnail yet = no image at all (an empty Image
                    // paints a black square); with one, the image shader
                    // owns the tile and the card skips its own paint.
                    let has_thumb = f32::from(u8::from(frame.is_some()));
                    let selected = f32::from(u8::from(entry.active));
                    let dropping = self.drop_target == Some(entry.asset);
                    cell.view(cx, ids!(grid_drop)).set_visible(cx, dropping);
                    let thumb = cell.image(cx, ids!(grid_thumb));
                    thumb.set_visible(cx, frame.is_some());
                    thumb.set_texture(cx, frame);
                    if let Some(mut thumb) = thumb.borrow_mut() {
                        thumb.draw_bg.set_dyn_instance(cx, live_id!(fill), &[fill]);
                        thumb.draw_bg.set_dyn_instance(cx, live_id!(img_aspect), &[aspect]);
                        thumb.draw_bg.set_dyn_instance(cx, live_id!(selected), &[selected]);
                    }
                    // The click's own feedback: spinner while the cue loads,
                    // a still red ring if it failed — and an effect tile
                    // still on placeholder art spins too: bake in progress.
                    let baking = entry_is_baking(entry);
                    let busy = cell.view(cx, ids!(grid_busy));
                    busy.set_visible(cx, entry.loading || entry.failed || baking);
                    if entry.loading || entry.failed || baking {
                        set_busy(cx, &busy, now, entry.failed);
                    }
                    if let Some(mut cell) = cell.borrow_mut() {
                        cell.draw_bg.set_dyn_instance(cx, live_id!(selected), &[selected]);
                        cell.draw_bg.set_dyn_instance(cx, live_id!(has_thumb), &[has_thumb]);
                    };
                }
                item.draw_all(cx, &mut Scope::empty());
                // Where the row's cells landed, now that they have. A carry
                // asks this for the tile it is over.
                for (slot, path) in slots.iter().enumerate() {
                    let index = row_id * cols + slot;
                    if slot >= cols || index >= self.entries.len() {
                        continue;
                    }
                    let area = item.view(cx, *path).area();
                    self.cell_areas.push((index, area));
                }
            }
        }
        self.pacer.arm(cx, now, demand);
        DrawStep::done()
    }
}


#[cfg(test)]
mod tile_fit_tests {
    use super::*;

    #[test]
    /// A tile whose thumbnail DECLARED cells shows its cell whole, whether
    /// the declaration held one cell or eight: the producer already fit the
    /// sprite into that square, so cropping into it cuts the sprite. Only a
    /// plain picture (a clip still, a photo) crops toward cover.
    fn declared_cell_tiles_are_contained_even_with_a_single_frame() {
        assert_eq!(fill_for_thumb(0, true), 0.0, "one-cell sprite strip");
        assert_eq!(fill_for_thumb(4, true), 0.0, "cycling sprite strip");
        assert_eq!(fill_for_thumb(4, false), 0.0, "any multi-frame sheet");
        assert_eq!(fill_for_thumb(0, false), TILE_CROP, "a plain still crops");
        assert_eq!(fill_for_thumb(1, false), TILE_CROP, "a still set frame-wise");
        // FIT is what a contained tile then does with the cell: the whole
        // square stays visible, bars on the wide axis of a 164x104 tile.
        let (w, h) = fit_scale(164.0 / 104.0, 1.0);
        assert!(w < 1.0 && h == 1.0, "a square cell letterboxes: {w} {h}");
        // What cropping would have cost: a quarter of the sprite's height.
        let (_, ch) = cover_scale(164.0 / 104.0, 1.0);
        assert!(ch > 1.5, "cover would grow the vertical axis past 1.5: {ch}");
    }

    #[test]
    /// The spinner's phase: a real angle that keeps turning and never
    /// leaves [0, tau), whatever the app clock says.
    fn the_busy_spinner_turns_and_stays_in_range() {
        let tau = std::f32::consts::TAU;
        for t in [0.0, 0.37, 5.0, 1234.5, 86_400.0] {
            let a = busy_spin(t);
            assert!((0.0..tau).contains(&a), "phase out of range at {t}: {a}");
        }
        // Turning, and slow enough to read: well under a turn in a frame.
        let step = busy_spin(1.0 / 60.0) - busy_spin(0.0);
        assert!(step > 0.0 && step < 0.2, "one frame of spin: {step}");
        // A tile that is loading has to keep redrawing; a still one need not.
        let mut entry = GridEntry {
            live: false,
            asset: AssetId::from_bytes([0; 16]),
            title: String::new(),
            sub: String::new(),
            state: String::new(),
            texture: None,
            frames: Vec::new(),
            fps: 0.0,
            cells: false,
            loading: false,
            failed: false,
            active: false,
            placeholder: false,
            pending: false,
            fx: false,
        };
        assert!(!entry_spins(&entry));
        entry.loading = true;
        assert!(entry_spins(&entry), "a spinner needs frames");
        entry.loading = false;
        entry.failed = true;
        assert!(!entry_spins(&entry), "a failed ring is still");
        entry.failed = false;
        entry.fx = true;
        entry.state = "FX".to_string();
        assert!(entry_is_baking(&entry), "an effect placeholder spins");
        entry.cells = true;
        assert!(
            !entry_is_baking(&entry),
            "a declared one-cell web bake is complete"
        );
    }

    #[test]
    fn cover_crops_a_wide_image_left_and_right_in_a_square_tile() {
        // A 2:1 image in a 1:1 tile: the wide axis must grow past 1 (it
        // samples a narrower band of the image, i.e. crops the sides) and
        // the other axis stays exactly 1 (uses the full image height).
        let (w, h) = cover_scale(1.0, 2.0);
        assert!(w > 1.0, "horizontal axis grows past 1 to crop: {w}");
        assert_eq!(h, 1.0);
    }

    #[test]
    fn cover_crops_a_tall_image_top_and_bottom_in_a_square_tile() {
        // A 1:2 (portrait) image in a 1:1 tile: the vertical axis crops.
        let (w, h) = cover_scale(1.0, 0.5);
        assert_eq!(w, 1.0);
        assert!(h > 1.0, "vertical axis grows past 1 to crop: {h}");
    }

    #[test]
    fn fit_letterboxes_a_wide_image_with_bars_top_and_bottom() {
        // Same wide image, same square tile: FIT bars the axis COVER
        // would otherwise have kept full (opposite of the crop case).
        let (w, h) = fit_scale(1.0, 2.0);
        assert_eq!(w, 1.0);
        assert!(h < 1.0, "vertical axis shrinks to letterbox: {h}");
    }

    #[test]
    fn fit_letterboxes_a_tall_image_with_bars_left_and_right() {
        let (w, h) = fit_scale(1.0, 0.5);
        assert!(w < 1.0, "horizontal axis shrinks to letterbox: {w}");
        assert_eq!(h, 1.0);
    }

    #[test]
    fn equal_aspects_are_the_identity_for_both_modes() {
        assert_eq!(cover_scale(16.0 / 9.0, 16.0 / 9.0), (1.0, 1.0));
        assert_eq!(fit_scale(16.0 / 9.0, 16.0 / 9.0), (1.0, 1.0));
        assert_eq!(cover_scale(1.0, 1.0), (1.0, 1.0));
        assert_eq!(fit_scale(1.0, 1.0), (1.0, 1.0));
    }

    #[test]
    fn cover_never_shrinks_and_fit_never_grows() {
        // For any non-degenerate pair, FIT's axes are always <= 1 (it only
        // ever shrinks to letterbox) and COVER's are always >= 1 (it only
        // ever grows to crop) — the `min` vs `max` inversion the task asks
        // for, checked across a spread of aspects.
        for (ra, ia) in [
            (1.0, 2.0),
            (1.0, 0.5),
            (16.0 / 9.0, 4.0 / 3.0),
            (9.0 / 16.0, 3.0),
            (4.0 / 3.0, 4.0 / 3.0),
        ] {
            let (fw, fh) = fit_scale(ra, ia);
            let (cw, ch) = cover_scale(ra, ia);
            assert!(fw <= 1.0 && fh <= 1.0, "fit never exceeds 1: {fw} {fh}");
            assert!(cw >= 1.0 && ch >= 1.0, "cover never goes below 1: {cw} {ch}");
        }
    }

    #[test]
    fn degenerate_inputs_are_guarded_not_nan_or_panicking() {
        for (rect_aspect, img_aspect) in [
            (0.0, 2.0),
            (2.0, 0.0),
            (0.0, 0.0),
            (f32::NAN, 1.0),
            (1.0, f32::NAN),
            (f32::NAN, f32::NAN),
            (-1.0, 2.0),
            (f32::INFINITY, 1.0),
            (1.0, f32::INFINITY),
        ] {
            let (fw, fh) = fit_scale(rect_aspect, img_aspect);
            let (cw, ch) = cover_scale(rect_aspect, img_aspect);
            assert!(
                fw.is_finite() && fh.is_finite() && cw.is_finite() && ch.is_finite(),
                "rect_aspect={rect_aspect} img_aspect={img_aspect} produced non-finite output"
            );
            assert!(fw > 0.0 && fh > 0.0 && cw > 0.0 && ch > 0.0);
        }
    }
}

// ---------------------------------------------------------------------------
// APC40 8×5 clip matrix
// ---------------------------------------------------------------------------

pub const PAD_ROWS: usize = 5;
pub const PAD_COLS: usize = 8;
pub const PAD_MATRIX: usize = PAD_ROWS * PAD_COLS;

/// The grid is one giant horizontal strip: entries fill a column top to
/// bottom, then the next column. An item keeps its row for good — the
/// window scrolls by whole COLUMNS (wheel / ◀ ▶ = one column, |◀ ▶| = a
/// page of PAD_COLS), every row moving together, never a per-row "snake".
/// `bank` is the first visible column.
pub fn pad_cols(len: usize) -> usize {
    len.div_ceil(PAD_ROWS)
}

/// Clamp a first-visible-column so the window never runs past the content.
pub fn clamp_pad_offset(first_col: usize, len: usize) -> usize {
    let cols = pad_cols(len);
    if cols <= PAD_COLS {
        return 0;
    }
    first_col.min(cols - PAD_COLS)
}

/// The entry-count basis every scroll computation (clamping, thumb size,
/// thumb position) should measure against: the catalog's reported TOTAL
/// once known, so the scrollbar represents the whole result set — not how
/// much of it has streamed in via `load_more()` yet. Before a total is
/// known (`total == 0`, e.g. a host that never wires one in) this falls
/// back to what is actually loaded, matching the old behavior.
pub fn scroll_basis(total: usize, loaded: usize) -> usize {
    if total > 0 {
        total
    } else {
        loaded
    }
}

/// Smallest thumb the operator can still grab, in pixels.
pub const SCROLL_THUMB_MIN: f64 = 24.0;

/// Scrollbar thumb geometry: `(offset_from_track_left, width)`.
///
/// Length is the visible fraction of the content (so a 777-column strip
/// gets a short thumb and a 9-column one a long thing that nearly fills
/// the track), floored at [`SCROLL_THUMB_MIN`] so it stays grabbable, and
/// the position maps the CURRENT first column onto the remaining travel.
pub fn scroll_thumb_geom(track_w: f64, len: usize, bank: usize) -> (f64, f64) {
    if track_w <= 0.0 {
        return (0.0, 0.0);
    }
    let total = pad_cols(len).max(1) as f64;
    let visible = (PAD_COLS as f64).min(total);
    let width = (track_w * (visible / total)).clamp(SCROLL_THUMB_MIN.min(track_w), track_w);
    let travel = (track_w - width).max(0.0);
    let max_off = clamp_pad_offset(usize::MAX, len);
    let t = if max_off == 0 {
        0.0
    } else {
        (bank.min(max_off) as f64) / max_off as f64
    };
    (travel * t, width)
}

/// Which column the thumb's LEFT edge at `thumb_x` corresponds to — the
/// inverse of [`scroll_thumb_geom`], for dragging.
pub fn scroll_offset_for_thumb(track_w: f64, len: usize, thumb_x: f64) -> usize {
    let (_, width) = scroll_thumb_geom(track_w, len, 0);
    let travel = (track_w - width).max(0.0);
    let max_off = clamp_pad_offset(usize::MAX, len);
    if travel <= 0.0 || max_off == 0 {
        return 0;
    }
    let t = (thumb_x / travel).clamp(0.0, 1.0);
    (t * max_off as f64).round() as usize
}

/// A press on the bare track (not the thumb) pages one visible-page of
/// columns toward wherever the operator clicked: `-1` (page left/back) if
/// the click landed before the thumb's left edge, `1` (page right/forward)
/// otherwise, including a click past the thumb's right edge.
pub fn scroll_page_dir(local_x: f64, thumb_x: f64) -> i32 {
    if local_x < thumb_x {
        -1
    } else {
        1
    }
}

/// Physical pad (row-major on the APC40 / screen: `row * PAD_COLS + slot`)
/// → entry index, given the first visible column.
pub fn pad_entry_index(first_col: usize, pad: usize) -> usize {
    let row = pad / PAD_COLS;
    let slot = pad % PAD_COLS;
    (first_col + slot) * PAD_ROWS + row
}

/// The entry pad `pad` paints from the bank at `first_col`: anything but a
/// reserved placeholder (pending clips and prefabs included).
fn pad_paints(entries: &[GridEntry], first_col: usize, pad: usize) -> Option<&GridEntry> {
    entries.get(pad_entry_index(first_col, pad)).filter(|entry| !entry.placeholder)
}

/// The entry a click on pad `pad` acts on: a painted one that is not
/// pending (see [`GridEntry::pending`]).
fn pad_acts_on(entries: &[GridEntry], first_col: usize, pad: usize) -> Option<&GridEntry> {
    pad_paints(entries, first_col, pad).filter(|entry| !entry.pending)
}

#[derive(Script, ScriptHook, Widget)]
pub struct VjPadMatrix {
    #[deref]
    view: View,
    #[live]
    draw_track: DrawQuad,
    #[live]
    draw_thumb: DrawQuad,
    #[rust]
    entries: Vec<GridEntry>,
    /// The catalog's reported result count across ALL pages (not just the
    /// ones that have streamed in). Once known, this — not `entries.len()`
    /// — is what the scrollbar sizes and clamps against, so the thumb does
    /// not shrink as `load_more()` pages arrive. Zero means "unknown yet"
    /// (no catalog wired in, e.g. plain unit-test construction): scrolling
    /// then falls back to the loaded count, same as before.
    #[rust]
    total: usize,
    #[rust]
    pub bank: usize,
    /// Last (selected, empty) pushed into each pad's shader: a script
    /// evaluation per cell per draw was the app's biggest per-frame cost.
    #[rust]
    cell_state: Vec<(f32, f32, f32)>,
    /// Last (fill, img_aspect) pushed into each pad's thumbnail shader —
    /// same rationale as `cell_state`: skip the script evaluation whenever
    /// the bound texture's mode/aspect hasn't actually changed.
    #[rust]
    thumb_state: Vec<(f32, f32, f32)>,
    #[rust]
    scroll_area: Area,
    /// Grab offset INSIDE the thumb while dragging (so the thumb does not
    /// jump to the cursor), or `None` when not dragging.
    #[rust]
    dragging: Option<f64>,
    #[rust]
    thumb_hover: bool,
    /// Trackpad deltas are fractional: accumulate to whole-column notches.
    #[rust]
    wheel_accum: f64,
    #[rust]
    pacer: WallPacer,
}

/// Wheel travel (px) per column step; a mouse notch is ~10–20 px, a
/// trackpad swipe many small deltas.
const WHEEL_NOTCH: f64 = 24.0;

impl VjPadMatrix {
    /// Entry-count basis for scroll clamping/geometry — see
    /// [`scroll_basis`]: the catalog TOTAL once known, else the loaded
    /// count.
    fn scroll_len(&self) -> usize {
        scroll_basis(self.total, self.entries.len())
    }

    /// The catalog's reported result total (across all pages). Call this
    /// whenever the host learns/updates it (e.g. from the first search
    /// page) — it does NOT need to be called again as more pages stream
    /// in via `load_more()`; the scrollbar stays put until the total
    /// itself changes (a new search, a refresh).
    pub fn set_total(&mut self, cx: &mut Cx, total: usize) {
        if self.total != total {
            self.total = total;
            self.bank = clamp_pad_offset(self.bank, self.scroll_len());
            self.view.redraw(cx);
        }
    }

    pub fn set_entries(&mut self, cx: &mut Cx, entries: Vec<GridEntry>) {
        self.entries = entries;
        self.bank = clamp_pad_offset(self.bank, self.scroll_len());
        self.view.redraw(cx);
    }

    /// Mark which tile is loading and which one failed, from the cue engine
    /// (or the pad loader) — pushed every pump rather than waiting for the
    /// next grid rebuild, because a click has to answer NOW and a rebuild
    /// only happens when the catalog changes.
    pub fn set_busy(&mut self, cx: &mut Cx, loading: Option<AssetId>, failed: Option<AssetId>) {
        let mut changed = false;
        for entry in &mut self.entries {
            let is_loading = loading == Some(entry.asset);
            let has_failed = failed == Some(entry.asset);
            if entry.loading != is_loading || entry.failed != has_failed {
                entry.loading = is_loading;
                entry.failed = has_failed;
                changed = true;
            }
        }
        if changed {
            self.view.redraw(cx);
        }
    }

    pub fn set_thumb(&mut self, cx: &mut Cx, asset: AssetId, texture: Texture) {
        self.set_thumb_anim(cx, asset, vec![texture], 0.0);
    }

    pub fn set_thumb_anim(&mut self, cx: &mut Cx, asset: AssetId, frames: Vec<Texture>, fps: f32) {
        let mut hit = false;
        for entry in &mut self.entries {
            if entry.asset == asset {
                entry.texture = frames.first().cloned();
                entry.frames = frames.clone();
                entry.fps = fps;
                hit = true;
            }
        }
        if hit {
            self.view.redraw(cx);
        }
    }

    pub fn visible_assets(&self) -> Vec<AssetId> {
        (0..40)
            .filter_map(|pad| self.visible_at(pad).map(|entry| entry.asset))
            .collect()
    }

    pub fn set_bank(&mut self, bank: usize) {
        self.bank = clamp_pad_offset(bank, self.scroll_len());
    }

    pub fn set_offset(&mut self, cx: &mut Cx, offset: usize) {
        let next = clamp_pad_offset(offset, self.scroll_len());
        if next != self.bank {
            self.bank = next;
            self.view.redraw(cx);
        }
    }

    /// Scroll the whole strip by `cols` columns (a page = PAD_COLS).
    pub fn nudge_cols(&mut self, cx: &mut Cx, cols: i32) {
        let next = (self.bank as i32 + cols).max(0) as usize;
        self.set_offset(cx, next);
    }

    pub fn entry_at(&self, index: usize) -> Option<&GridEntry> {
        self.entries.get(index)
    }

    /// The entry a pad ACTS on: a prefab cell is drawn but not yet clickable
    /// (see [`GridEntry::pending`]), so it is not one.
    pub fn visible_at(&self, pad: usize) -> Option<&GridEntry> {
        pad_acts_on(&self.entries, self.bank, pad)
    }

    /// The entry a pad PAINTS — prefabs included.
    pub fn paint_at(&self, pad: usize) -> Option<&GridEntry> {
        pad_paints(&self.entries, self.bank, pad)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// The window shows the tail of what is loaded: the host should page
    /// the catalog (server-side) rather than let the bank run dry.
    pub fn at_tail(&self) -> bool {
        (self.bank + PAD_COLS) * PAD_ROWS >= self.entries.len()
    }

    /// Push every pad's entry into its cell, and report what the pads that
    /// were painted need from the clock.
    fn bind_pads(&mut self, cx: &mut Cx, now: f64) -> WallDemand {
        let mut demand = WallDemand::default();
        let rows = [ids!(r0), ids!(r1), ids!(r2), ids!(r3), ids!(r4)];
        let slots = [
            ids!(c1),
            ids!(c2),
            ids!(c3),
            ids!(c4),
            ids!(c5),
            ids!(c6),
            ids!(c7),
            ids!(c8),
        ];
        for (row, row_id) in rows.iter().enumerate() {
            let row_view = self.view.view(cx, *row_id);
            for (slot, path) in slots.iter().enumerate() {
                let pad = row * PAD_COLS + slot;
                let mut cell = row_view.view(cx, *path);
                let (selected, empty, has_thumb, fill, aspect) = if let Some(entry) = self.paint_at(pad) {
                    cell.label(cx, ids!(grid_title)).set_text(cx, &entry.title);
                    let sub = cell.label(cx, ids!(grid_sub));
                    sub.set_visible(cx, !entry.sub.is_empty());
                    sub.set_text(cx, &entry.sub);
                    demand.note(entry, now);
                    let frame = entry_frame(entry, now, pad);
                    let aspect = thumb_aspect(cx, frame.as_ref());
                    let fill = thumb_fill(entry);
                    // No thumbnail yet = no image at all (an empty Image
                    // paints a black square). With a thumb the IMAGE
                    // shader owns the whole tile (single-fill law) and
                    // the card skips its own paint — `has_thumb` below.
                    let has_thumb = f32::from(u8::from(frame.is_some()));
                    let image = cell.image(cx, ids!(grid_thumb));
                    image.set_visible(cx, frame.is_some());
                    image.set_texture(cx, frame);
                    // The grid's ONLY state paint: a green ring on the tile
                    // last clicked. LIVE / CUE / HOLD live in the program
                    // strip's labels, not on forty tiles at once.
                    let selected = f32::from(u8::from(entry.active));
                    // The click's own feedback: spinner while this pad's cue
                    // loads, a still red ring if it failed. An effect tile
                    // still on its placeholder art gets the same spinner —
                    // the bake is in progress, and a tile that visibly moves
                    // says so (a static placeholder reads as done-and-boring).
                    let baking = entry_is_baking(entry);
                    let busy = cell.view(cx, ids!(grid_busy));
                    busy.set_visible(cx, entry.loading || entry.failed || baking);
                    if entry.loading || entry.failed || baking {
                        set_busy(cx, &busy, now, entry.failed);
                    }
                    (selected, 0.0, has_thumb, fill, aspect)
                } else {
                    // No content: a quiet, greyed placeholder, not a black pad.
                    cell.label(cx, ids!(grid_title)).set_text(cx, "");
                    let sub = cell.label(cx, ids!(grid_sub));
                    sub.set_visible(cx, false);
                    sub.set_text(cx, "");
                    let image = cell.image(cx, ids!(grid_thumb));
                    image.set_visible(cx, false);
                    image.set_texture(cx, None);
                    cell.view(cx, ids!(grid_busy)).set_visible(cx, false);
                    (0.0, 1.0, 0.0, 1.0, 1.0)
                };
                if self.cell_state.len() <= pad {
                    self.cell_state.resize(pad + 1, (-1.0, -1.0, -1.0));
                }
                if self.cell_state[pad] != (selected, empty, has_thumb) {
                    self.cell_state[pad] = (selected, empty, has_thumb);
                    script_apply_eval!(cx, cell, {
                        draw_bg +: { selected: #(selected) empty: #(empty) has_thumb: #(has_thumb) }
                    });
                }
                if self.thumb_state.len() <= pad {
                    self.thumb_state.resize(pad + 1, (-1.0, -1.0, -1.0));
                }
                if self.thumb_state[pad] != (fill, aspect, selected) {
                    self.thumb_state[pad] = (fill, aspect, selected);
                    let mut thumb = cell.image(cx, ids!(grid_thumb));
                    script_apply_eval!(cx, thumb, {
                        draw_bg +: { fill: #(fill) img_aspect: #(aspect) selected: #(selected) }
                    });
                }
            }
        }
        demand
    }
}

impl Widget for VjPadMatrix {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if self.pacer.fired(event) {
            self.view.redraw(cx);
        }
        if let Event::Scroll(scroll) = event {
            // Wheel over the strip = one column per notch, like ◀ ▶. A
            // horizontal trackpad swipe scrolls too; either axis claims
            // the event so the pane behind does not scroll as well.
            if self.view.area().rect(cx).contains(scroll.abs) {
                let delta = if scroll.scroll.x.abs() > scroll.scroll.y.abs() {
                    scroll.scroll.x
                } else {
                    scroll.scroll.y
                };
                self.wheel_accum += delta;
                let mut step = 0;
                while self.wheel_accum >= WHEEL_NOTCH {
                    self.wheel_accum -= WHEEL_NOTCH;
                    step += 1;
                }
                while self.wheel_accum <= -WHEEL_NOTCH {
                    self.wheel_accum += WHEEL_NOTCH;
                    step -= 1;
                }
                if step != 0 {
                    self.nudge_cols(cx, step);
                }
                scroll.handled_x.set(true);
                scroll.handled_y.set(true);
            }
        }
        match event.hits(cx, self.scroll_area) {
            Hit::FingerDown(fe) if fe.is_primary_hit() => {
                let track = self.scroll_area.rect(cx);
                let (thumb_x, thumb_w) =
                    scroll_thumb_geom(track.size.x, self.scroll_len(), self.bank);
                let local = fe.abs.x - track.pos.x;
                if local >= thumb_x && local <= thumb_x + thumb_w {
                    // Grabbed the thumb: keep the grab point under the
                    // cursor instead of snapping the thumb to it.
                    self.dragging = Some(local - thumb_x);
                } else {
                    // Clicked the bare track: page toward the click.
                    let dir = scroll_page_dir(local, thumb_x);
                    self.nudge_cols(cx, dir * PAD_COLS as i32);
                }
                self.view.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                if let Some(grab) = self.dragging {
                    let track = self.scroll_area.rect(cx);
                    let want = fe.abs.x - track.pos.x - grab;
                    let offset =
                        scroll_offset_for_thumb(track.size.x, self.scroll_len(), want);
                    self.set_offset(cx, offset);
                }
            }
            Hit::FingerUp(_) => {
                self.dragging = None;
                self.view.redraw(cx);
            }
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let track = self.scroll_area.rect(cx);
                let (thumb_x, thumb_w) =
                    scroll_thumb_geom(track.size.x, self.scroll_len(), self.bank);
                let local = fe.abs.x - track.pos.x;
                let over = local >= thumb_x && local <= thumb_x + thumb_w;
                if over != self.thumb_hover {
                    self.thumb_hover = over;
                    self.view.redraw(cx);
                }
                cx.set_cursor(MouseCursor::Hand);
            }
            Hit::FingerHoverOut(_) => {
                if self.thumb_hover {
                    self.thumb_hover = false;
                    self.view.redraw(cx);
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let now = cx.seconds_since_app_start();
        let demand = self.bind_pads(cx, now);
        self.pacer.arm(cx, now, demand);
        let step = self.view.draw_walk(cx, scope, walk);
        let slot = self.view.view(cx, ids!(scroll_slot)).area().rect(cx);
        if slot.size.x > 1.0 {
            self.draw_track.draw_abs(cx, slot);
            // The TRACK's own quad is the hit surface, and it covers the
            // whole slot — so a press anywhere on the bar is ours.
            self.scroll_area = self.draw_track.area();
            let (thumb_x, thumb_w) = scroll_thumb_geom(slot.size.x, self.scroll_len(), self.bank);
            self.draw_thumb.set_uniform(cx, id!(hover), &[f32::from(u8::from(self.thumb_hover))]);
            self.draw_thumb.set_uniform(
                cx,
                id!(down),
                &[f32::from(u8::from(self.dragging.is_some()))],
            );
            self.draw_thumb.draw_abs(
                cx,
                Rect {
                    pos: dvec2(slot.pos.x + thumb_x, slot.pos.y),
                    size: dvec2(thumb_w, slot.size.y),
                },
            );
        }
        step
    }
}

#[cfg(test)]
mod pad_matrix_tests {
    use super::*;

    #[test]
    fn scrollbar_thumb_shows_how_much_content_there_is() {
        let track = 400.0;
        // Everything fits: a full-length thumb parked at the left.
        let (x, w) = scroll_thumb_geom(track, 40, 0);
        assert_eq!((x, w), (0.0, track), "8 columns of 8 fill the track");
        // Half the strip visible → half the track, at the left.
        let (x, w) = scroll_thumb_geom(track, PAD_ROWS * 16, 0);
        assert!((w - 200.0).abs() < 1e-6, "8 of 16 columns → half a track: {w}");
        assert_eq!(x, 0.0);
        // …and at the far right end when scrolled to the tail.
        let last = clamp_pad_offset(usize::MAX, PAD_ROWS * 16);
        assert_eq!(last, 8);
        let (x, w) = scroll_thumb_geom(track, PAD_ROWS * 16, last);
        assert!((x + w - track).abs() < 1e-6, "the tail parks the thumb at the end");
        // Past the end clamps rather than running off the track.
        let (x_over, _) = scroll_thumb_geom(track, PAD_ROWS * 16, 9999);
        assert_eq!(x_over, x);
        // A huge strip still leaves something grabbable.
        let (_, w) = scroll_thumb_geom(track, PAD_ROWS * 4000, 0);
        assert_eq!(w, SCROLL_THUMB_MIN, "tiny fraction floors at the min size");
        // A track narrower than the minimum thumb never overflows.
        let (_, w) = scroll_thumb_geom(12.0, PAD_ROWS * 4000, 0);
        assert!(w <= 12.0, "thumb fits its track: {w}");
        assert_eq!(scroll_thumb_geom(0.0, 100, 0), (0.0, 0.0));
    }

    #[test]
    fn dragging_the_thumb_maps_back_to_columns() {
        let track = 400.0;
        let len = PAD_ROWS * 16; // 16 columns, 8 visible, 8 of travel
        assert_eq!(scroll_offset_for_thumb(track, len, 0.0), 0);
        assert_eq!(scroll_offset_for_thumb(track, len, -50.0), 0, "clamps left");
        assert_eq!(scroll_offset_for_thumb(track, len, 999.0), 8, "clamps right");
        // Half the travel is half the columns.
        let (_, w) = scroll_thumb_geom(track, len, 0);
        let travel = track - w;
        assert_eq!(scroll_offset_for_thumb(track, len, travel * 0.5), 4);
        // Round trip: every column maps to a thumb position that maps back.
        for bank in 0..=8 {
            let (x, _) = scroll_thumb_geom(track, len, bank);
            assert_eq!(scroll_offset_for_thumb(track, len, x), bank, "bank {bank}");
        }
        // Nothing to scroll: every drag stays at zero.
        assert_eq!(scroll_offset_for_thumb(track, 40, 300.0), 0);
    }

    #[test]
    fn strip_scrolls_by_whole_columns_and_rows_stay_put() {
        // 50 entries = 10 columns of 5; the 8-column window stops at col 2.
        assert_eq!(pad_cols(50), 10);
        assert_eq!(clamp_pad_offset(0, 50), 0);
        assert_eq!(clamp_pad_offset(100, 50), 2);
        assert_eq!(clamp_pad_offset(0, 10), 0);
        // Column order: the pad under pad 0 (next row) is the next entry;
        // the pad to the right starts the next column.
        assert_eq!(pad_entry_index(0, 0), 0);
        assert_eq!(pad_entry_index(0, PAD_COLS), 1);
        assert_eq!(pad_entry_index(0, 1), PAD_ROWS);
        // Scrolling one column: every row keeps its item's row.
        assert_eq!(pad_entry_index(1, 0), PAD_ROWS);
        assert_eq!(pad_entry_index(1, PAD_COLS + 1), 2 * PAD_ROWS + 1);
    }

    #[test]
    fn thumb_fills_track_when_content_is_a_single_page() {
        // 20 entries = 4 columns, well under the 8-column window: nothing
        // to scroll, so the thumb should be the full track and undraggable
        // (zero travel).
        let (x, w) = scroll_thumb_geom(200.0, 20, 0);
        assert_eq!(x, 0.0);
        assert_eq!(w, 200.0);
        assert_eq!(clamp_pad_offset(usize::MAX, 20), 0);
    }

    #[test]
    fn thumb_clamps_to_the_minimum_when_content_is_huge() {
        // 100_000 entries = 20_000 columns; the proportional width
        // (8/20_000 of 200px ~= 0.08px) must floor at SCROLL_THUMB_MIN.
        let (_, w) = scroll_thumb_geom(200.0, 100_000, 0);
        assert_eq!(w, SCROLL_THUMB_MIN);
    }

    #[test]
    fn thumb_position_tracks_the_column_offset() {
        // 50 entries = 10 columns; window of 8 leaves 2 columns of travel.
        let track_w = 200.0;
        let len = 50;
        let max_off = clamp_pad_offset(usize::MAX, len);
        assert_eq!(max_off, 2);

        let (x0, w) = scroll_thumb_geom(track_w, len, 0);
        assert_eq!(x0, 0.0);

        let (x_max, w_max) = scroll_thumb_geom(track_w, len, max_off);
        assert_eq!(w_max, w);
        assert_eq!(x_max, track_w - w);
    }

    #[test]
    fn drag_math_round_trips_through_every_offset() {
        // For every reachable column offset, converting it to a thumb
        // position and back must land on the exact same offset (pixel
        // math must not drift the drag around).
        let track_w = 240.0;
        let len = 137; // an offbeat count, not a multiple of anything
        let max_off = clamp_pad_offset(usize::MAX, len);
        assert!(max_off > 0, "fixture should actually be scrollable");
        for offset in 0..=max_off {
            let (x, _) = scroll_thumb_geom(track_w, len, offset);
            let back = scroll_offset_for_thumb(track_w, len, x);
            assert_eq!(back, offset, "offset {offset} did not round-trip");
        }
    }

    #[test]
    fn drag_math_clamps_positions_outside_the_track() {
        let track_w = 200.0;
        let len = 50;
        let max_off = clamp_pad_offset(usize::MAX, len);
        // Dragging the thumb's left edge past either end of the track
        // clamps to the first/last column, it does not panic or wrap.
        assert_eq!(scroll_offset_for_thumb(track_w, len, -500.0), 0);
        assert_eq!(scroll_offset_for_thumb(track_w, len, 5_000.0), max_off);
    }

    #[test]
    fn track_click_pages_toward_the_click_on_either_side() {
        let (thumb_x, thumb_w) = scroll_thumb_geom(200.0, 50, 4);
        // Click left of the thumb: page back.
        assert_eq!(scroll_page_dir(thumb_x - 10.0, thumb_x), -1);
        // Click right of the thumb (past its trailing edge): page forward.
        assert_eq!(scroll_page_dir(thumb_x + thumb_w + 10.0, thumb_x), 1);
    }

    #[test]
    fn scroll_geometry_guards_against_division_by_zero() {
        // Zero-width track: nothing to divide by, must not panic or NaN.
        let (x, w) = scroll_thumb_geom(0.0, 500, 0);
        assert_eq!((x, w), (0.0, 0.0));
        assert_eq!(scroll_offset_for_thumb(0.0, 500, 10.0), 0);

        // Empty content: pad_cols(0) == 0, still no panic/NaN, and the
        // thumb fills the (degenerate) track.
        let (x0, w0) = scroll_thumb_geom(150.0, 0, 0);
        assert_eq!(x0, 0.0);
        assert_eq!(w0, 150.0);
        assert_eq!(scroll_offset_for_thumb(150.0, 0, 75.0), 0);

        // A track narrower than the minimum thumb width must not panic on
        // an inverted `clamp(min, max)` range.
        let (_, w_narrow) = scroll_thumb_geom(10.0, 100_000, 0);
        assert_eq!(w_narrow, 10.0);
    }

    // -----------------------------------------------------------------
    // Regression: the thumb must size off the catalog TOTAL, not how many
    // pages `load_more()` has streamed in so far — otherwise it visibly
    // shrinks mid-drag as more pages land.
    // -----------------------------------------------------------------

    #[test]
    fn thumb_geometry_is_identical_before_and_after_pages_load() {
        // Same total (500), only the loaded count differs: one page in
        // (50) vs. everything loaded (500). `scroll_basis` must collapse
        // both to the same number, so every downstream computation is
        // byte-for-byte identical.
        let total = 500;
        let basis_first_page = scroll_basis(total, 50);
        let basis_fully_loaded = scroll_basis(total, 500);
        assert_eq!(basis_first_page, total);
        assert_eq!(basis_first_page, basis_fully_loaded);

        let track_w = 240.0;
        for bank in [0, 5, 50, 92] {
            assert_eq!(
                scroll_thumb_geom(track_w, basis_first_page, bank),
                scroll_thumb_geom(track_w, basis_fully_loaded, bank),
                "bank {bank} must not move/resize the thumb as pages stream in"
            );
        }
    }

    #[test]
    fn thumb_geometry_changes_only_when_total_itself_changes() {
        let track_w = 240.0;
        // Loaded count swinging from 10 to 400 with the SAME total: no
        // change (the case above generalized to width alone).
        let (_, w_before) = scroll_thumb_geom(track_w, scroll_basis(500, 10), 0);
        let (_, w_after) = scroll_thumb_geom(track_w, scroll_basis(500, 400), 0);
        assert_eq!(w_before, w_after);

        // A genuinely different total (a new search/refresh) DOES change
        // it.
        let (_, w_new_search) = scroll_thumb_geom(track_w, scroll_basis(120, 10), 0);
        assert_ne!(w_before, w_new_search);
    }

    #[test]
    fn max_offset_and_far_end_position_derive_from_total_not_loaded_count() {
        let track_w = 240.0;
        let total = 500;
        let loaded = 50; // only the first page has streamed in
        let basis = scroll_basis(total, loaded);
        assert_eq!(basis, total, "the loaded count must not leak into the basis");

        // total=500, rows=PAD_ROWS(5) -> total_cols = ceil(500/5) = 100;
        // visible=PAD_COLS(8) -> max_offset = 100 - 8 = 92.
        let max_off = clamp_pad_offset(usize::MAX, basis);
        assert_eq!(max_off, pad_cols(total) - PAD_COLS);
        assert_eq!(max_off, 92);

        // Dragging to the far end must reach the LAST column of the whole
        // catalog result, i.e. x == track_w - thumb_w, even though only
        // one page (50 of 500) is actually loaded.
        let (x, w) = scroll_thumb_geom(track_w, basis, max_off);
        assert_eq!(x, track_w - w);

        // And it must be a small (min-clamped) thumb, not one sized off
        // the 50 loaded entries (10 columns, which would fill most of the
        // track instead of floor at the minimum).
        assert_eq!(w, SCROLL_THUMB_MIN);
    }

    fn entry(byte: u8, pending: bool, loading: bool, failed: bool) -> GridEntry {
        GridEntry {
            asset: AssetId::from_bytes([byte; 16]),
            title: format!("clip {byte}"),
            sub: if pending { "queued".into() } else { "node 42 · fasth3-4step-q4-24g".into() },
            state: String::new(),
            texture: None,
            frames: Vec::new(),
            fps: 0.0,
            cells: false,
            loading,
            failed,
            active: false,
            live: false,
            placeholder: false,
            pending,
            fx: false,
        }
    }

    #[test]
    fn pending_pad_entries_prepend_and_stay_unclickable() {
        // A generating clip and a failed one head the strip, then two
        // published clips. Pads fill column by column: pad 0 is entry 0,
        // pad PAD_COLS (the row below) is entry 1, and so on. The matrix's
        // `paint_at` / `visible_at` are exactly these two functions.
        let entries = vec![
            entry(1, true, true, false),
            entry(2, true, false, true),
            entry(3, false, false, false),
            entry(4, false, false, false),
        ];
        let paint_at = |pad| pad_paints(&entries, 0, pad);
        let visible_at = |pad| pad_acts_on(&entries, 0, pad);
        let pads: Vec<usize> = (0..4).map(|row| row * PAD_COLS).collect();
        // The pending pads paint, flags intact…
        let running = paint_at(pads[0]).expect("a running clip paints");
        assert!(running.pending && running.loading && !running.failed);
        assert_eq!(running.sub, "queued");
        assert!(entry_spins(running), "a running clip's ring turns");
        let failed = paint_at(pads[1]).expect("a failed clip paints");
        assert!(failed.pending && failed.failed && !failed.loading);
        assert!(!entry_spins(failed), "a failed ring is still");
        // …but no click acts on them.
        assert!(visible_at(pads[0]).is_none());
        assert!(visible_at(pads[1]).is_none());
        // The published clips below them are clickable, captions bound.
        let published = visible_at(pads[2]).expect("a published clip acts");
        assert_eq!(published.asset, AssetId::from_bytes([3; 16]));
        assert_eq!(published.sub, "node 42 · fasth3-4step-q4-24g");
        assert_eq!(visible_at(pads[3]).map(|e| e.asset), Some(AssetId::from_bytes([4; 16])));
        // A pad past the end paints nothing.
        assert!(paint_at(1).is_none());
    }
}

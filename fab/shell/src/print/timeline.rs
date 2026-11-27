//! The slice view's controls, drawn over the plate:
//!
//! * [`FabMoveSlider`] — the moves within the current layer, left to right:
//!   drag to put the nozzle anywhere along the layer's path.
//! * [`FabSlicePad`] — the playback's pad, a Kaoss pad's square: up it the layers (layer 1 at the
//!   foot), across it the moves within the layer; a press or a drag puts the playhead at any
//!   moment of the print, a glowing point where it is. The round handle on its rail sets the
//!   lowest layer shown; amber lines mark the pauses for items placed in. A resin printer's
//!   stack walks up it alone (its layers cure at once).
//! * [`FabSliceLegend`] — what the colours mean in the current colour mode,
//!   with the totals: time and path length per feature, print time, the
//!   filament each slot uses and its cost when known.
//! * [`FabIssueSummary`] — the printability report under it: one line per
//!   kind of issue with its count and severity; a press shows the first of
//!   them in the view, the buttons beside its count (or the arrow keys) step
//!   through the rest, "3 / 8"; the detail is in the hover tip.
//!
//! * [`FabJobTimeline`] — the printer's job along its layers: done, the layer
//!   printing, still to print, with the printer's layer, progress and time left.
//!
//! All of them map the pointer to an absolute position (no deltas) and edit
//! only through [`PrintAction`]s.

use super::*;
use super::trays::DrawFabChip;
use super::colour_map::{ellipsis, warning_mark};
use super::stepper::{self, RowPart};
use super::colours::{colour_word, dist, hex, mapping_text, UNASSIGNED_TIP, VISIBLE_DIFF};
use crate::api::AppState;
use makepad_widgets_core::tip::{TipAction, TipPlace, TipRequest};
use makepad_widgets_core::property::PropHint;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.draw.DrawFabRange = mod.std.set_type_default() do #(DrawFabRange::script_shader(vm)){
        ..mod.draw.DrawQuad
        // Fractions along the track: the shown range and the playhead.
        lo: 0.0
        hi: 1.0
        head: 1.0
        vertical: 0.0
        hover: 0.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            if self.vertical > 0.5 {
                let cx = w * 0.5
                let top = 10.0
                let len = h - 20.0
                // Track, then the shown range (bottom = layer 1).
                sdf.box(cx - 2.5, top, 5.0, len, 1.25)
                sdf.fill(vec4(0.0, 0.0, 0.0, 0.35))
                let y_hi = top + len * (1.0 - self.hi)
                let y_lo = top + len * (1.0 - self.lo)
                sdf.box(cx - 2.5, y_hi, 5.0, max(y_lo - y_hi, 1.0), 1.25)
                sdf.fill(fab.color_accent)
                sdf.box(cx - 8.0, y_hi - 5.0, 16.0, 10.0, 2.0)
                sdf.fill_keep(fab.color_text_active)
                sdf.stroke(vec4(0.0, 0.0, 0.0, 0.5), 1.0)
                sdf.box(cx - 6.0, y_lo - 3.0, 12.0, 6.0, 1.5)
                sdf.fill_keep(fab.color_text_dim)
                sdf.stroke(vec4(0.0, 0.0, 0.0, 0.5), 1.0)
                return sdf.result
            }
            let cy = h * 0.5
            let left = 7.0
            let len = w - 14.0
            sdf.box(left, cy - 2.0, len, 4.0, 1.0)
            sdf.fill(vec4(0.0, 0.0, 0.0, 0.35))
            let x = left + len * self.head
            sdf.box(left, cy - 2.0, max(x - left, 1.0), 4.0, 1.0)
            sdf.fill(fab.color_accent)
            sdf.circle(x, cy, 6.0 + self.hover)
            sdf.fill_keep(fab.color_text_active)
            sdf.stroke(vec4(0.0, 0.0, 0.0, 0.5), 1.0)
            return sdf.result
        }
    }

    mod.draw.DrawFabRamp = mod.std.set_type_default() do #(DrawFabRamp::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, 1.5)
            let x = self.pos.x
            let c = vec3(0.15, 0.30, 0.95).mix(vec3(0.10, 0.80, 0.85), clamp(x * 4.0, 0.0, 1.0))
                .mix(vec3(0.20, 0.85, 0.25), clamp(x * 4.0 - 1.0, 0.0, 1.0))
                .mix(vec3(0.95, 0.85, 0.15), clamp(x * 4.0 - 2.0, 0.0, 1.0))
                .mix(vec3(0.95, 0.20, 0.15), clamp(x * 4.0 - 3.0, 0.0, 1.0))
            sdf.fill(vec4(c, 1.0))
            return sdf.result
        }
    }

    mod.draw.DrawFabJobBar = mod.std.set_type_default() do #(DrawFabJobBar::script_shader(vm)){
        ..mod.draw.DrawQuad
        // Fractions along the bar: the layers done and the end of the layer printing.
        done: 0.0
        cur: 0.0
        layers: 1.0
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let w = self.rect_size.x
            let h = self.rect_size.y
            sdf.box(0.0, 0.0, w, h, 2.0)
            sdf.fill(vec4(0.0, 0.0, 0.0, 0.40))
            if self.done > 0.0 {
                sdf.box(0.0, 0.0, max(w * self.done, 2.0), h, 2.0)
                sdf.fill(vec4(fab.color_accent.xyz * 0.85, 1.0))
            }
            if self.cur > self.done {
                sdf.box(w * self.done, 0.0, max(w * (self.cur - self.done), 3.0), h, 1.0)
                sdf.fill(fab.color_text_active)
            }
            var c = sdf.result
            // One tick per layer while they are at least 5 px apart.
            let px = w / max(self.layers, 1.0)
            let x = self.pos.x * w
            if px >= 5.0 && x - floor(x / px) * px < 1.0 && x > 1.0 {
                c = vec4(c.xyz * 0.55, c.w)
            }
            return c
        }
    }

    mod.widgets.FabJobTimelineBase = #(FabJobTimeline::register_widget(vm))
    mod.widgets.FabJobTimeline = set_type_default() do mod.widgets.FabJobTimelineBase{
        width: Fill
        height: 30
        draw_bar: mod.draw.DrawFabJobBar{}
        draw_text: mod.draw.DrawText{
            color: fab.color_text_active
            text_style: theme.font_bold{ font_size: fab.font_size_small }
        }
        draw_dim: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
    }

    mod.draw.DrawFabPad = mod.std.set_type_default() do #(DrawFabPad::script_shader(vm)){
        ..mod.draw.DrawQuad
        // the point (fractions: across the layer's moves, up the layers), the floor (the lowest
        // layer shown, its handle on the rail at the right), a press, the pointer over the pad and
        // over the floor's handle, the rail's width, and a resin stack (layers only: no moves)
        px: 0.0
        py: 0.0
        lo: 0.0
        down: 0.0
        hover: 0.0
        floor_hot: 0.0
        rail: 18.0
        resin: 0.0
        pixel: fn() {
            let p = self.pos * self.rect_size
            let sdf = Sdf2d.viewport(p)
            let w = self.rect_size.x - self.rail
            let h = self.rect_size.y
            let a = fab.color_accent
            // the well, a step darker than the card
            sdf.box(0.5, 0.5, w - 1.0, h - 1.0, 5.0)
            sdf.fill_keep(vec4(0.0, 0.0, 0.0, 0.32))
            sdf.stroke(vec4(1.0, 1.0, 1.0, 0.06 + 0.06 * self.hover), 1.0)
            let inset = 6.0
            let iw = w - inset * 2.0
            let ih = h - inset * 2.0
            // the layers cut away below the floor, shaded
            sdf.rect(inset, inset + ih * (1.0 - self.lo), iw, ih * self.lo)
            sdf.fill(vec4(0.0, 0.0, 0.0, 0.25))
            // a quarter grid
            let g = vec4(1.0, 1.0, 1.0, 0.045)
            sdf.rect(inset + iw * 0.25, inset, 1.0, ih)
            sdf.fill(g)
            sdf.rect(inset + iw * 0.5, inset, 1.0, ih)
            sdf.fill(g)
            sdf.rect(inset + iw * 0.75, inset, 1.0, ih)
            sdf.fill(g)
            sdf.rect(inset, inset + ih * 0.25, iw, 1.0)
            sdf.fill(g)
            sdf.rect(inset, inset + ih * 0.5, iw, 1.0)
            sdf.fill(g)
            sdf.rect(inset, inset + ih * 0.75, iw, 1.0)
            sdf.fill(g)
            // the printed share of the layer, and the crosshair through the point (a resin layer
            // cures at once: its line alone)
            let x = mix(inset + iw * self.px, inset + iw * 0.5, self.resin)
            let y = inset + ih * (1.0 - self.py)
            sdf.rect(inset, y - 1.0, mix(x - inset, iw, self.resin), 2.0)
            sdf.fill(vec4(a.xyz, 0.55))
            sdf.rect(x - 0.5, inset, 1.0, ih)
            sdf.fill(vec4(a.xyz, 0.22 * (1.0 - self.resin)))
            sdf.rect(inset, y - 0.5, iw, 1.0)
            sdf.fill(vec4(a.xyz, 0.22))
            // the point's glow, wider while pressed
            sdf.circle(x, y, 16.0 + 8.0 * self.down)
            sdf.fill(vec4(a.xyz, 0.08 + 0.08 * self.down))
            sdf.circle(x, y, 9.0 + 3.0 * self.down)
            sdf.fill(vec4(a.xyz, 0.18 + 0.12 * self.down))
            sdf.circle(x, y, 5.0)
            sdf.fill_keep(fab.color_text_active)
            sdf.stroke(vec4(0.0, 0.0, 0.0, 0.5), 1.0)
            // the floor's rail at the right, its round handle at the lowest layer shown
            let rx = w + self.rail * 0.5
            sdf.box(rx - 1.5, inset, 3.0, ih, 1.5)
            sdf.fill(vec4(0.0, 0.0, 0.0, 0.35))
            let fy = inset + ih * (1.0 - self.lo)
            sdf.box(rx - 1.5, inset + ih * (1.0 - self.py), 3.0, ih * max(self.py - self.lo, 0.0), 1.5)
            sdf.fill(vec4(a.xyz, 0.45))
            sdf.circle(rx, fy, 5.5 + 1.0 * self.floor_hot)
            sdf.fill_keep(mix(fab.color_text_dim, fab.color_text_active, self.floor_hot))
            sdf.stroke(vec4(0.0, 0.0, 0.0, 0.5), 1.0)
            return sdf.result
        }
    }

    mod.widgets.FabSlicePadBase = #(FabSlicePad::register_widget(vm))
    mod.widgets.FabSlicePad = set_type_default() do mod.widgets.FabSlicePadBase{
        width: Fill
        height: 132
        draw_pad: mod.draw.DrawFabPad{}
        // the pauses' marks across the pad, in amber
        draw_mark: mod.draw.DrawFabChip{ radius: 1.0 }
    }

    mod.widgets.FabMoveSliderBase = #(FabMoveSlider::register_widget(vm))
    mod.widgets.FabMoveSlider = set_type_default() do mod.widgets.FabMoveSliderBase{
        width: Fill
        height: 22
        draw_bar: mod.draw.DrawFabRange{ vertical: 0.0 }
    }

    mod.widgets.FabIssueSummaryBase = #(FabIssueSummary::register_widget(vm))
    mod.widgets.FabIssueSummary = set_type_default() do mod.widgets.FabIssueSummaryBase{
        width: Fill
        height: Fit
        draw_chip: mod.draw.DrawFabChip{ radius: 3.0 }
        draw_hit: mod.draw.DrawFabChip{ radius: 0.0 }
        draw_text: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_dim: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_head: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_bold{ font_size: fab.font_size_small }
        }
        draw_prev: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/chevron_left.svg") color: fab.color_text_dim }
        draw_next: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/chevron_right.svg") color: fab.color_text_dim }
    }

    mod.widgets.FabSliceLegendBase = #(FabSliceLegend::register_widget(vm))
    mod.widgets.FabSliceLegend = set_type_default() do mod.widgets.FabSliceLegendBase{
        width: Fill
        height: Fit
        color_text: fab.color_text
        color_dim: fab.color_text_dim
        color_border: fab.color_border_light
        color_warning: fab.color_warning
        color_error: fab.color_error
        draw_warn: mod.draw.DrawText{
            color: fab.color_editor
            ink_centered: true
            text_style: theme.font_bold{ font_size: fab.font_size_small }
        }
        draw_chip: mod.draw.DrawFabChip{ radius: 1.5 }
        draw_ramp: mod.draw.DrawFabRamp{}
        draw_text: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_dim: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_head: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_bold{ font_size: fab.font_size_small }
        }
    }

}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabRange {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    lo: f32,
    #[live]
    hi: f32,
    #[live]
    head: f32,
    #[live]
    vertical: f32,
    #[live]
    hover: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabPad {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    px: f32,
    #[live]
    py: f32,
    #[live]
    lo: f32,
    #[live]
    down: f32,
    #[live]
    hover: f32,
    #[live]
    floor_hot: f32,
    #[live]
    rail: f32,
    #[live]
    resin: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabJobBar {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    done: f32,
    #[live]
    cur: f32,
    #[live]
    layers: f32,
}

#[derive(Script, ScriptHook, Debug)]
#[repr(C)]
pub struct DrawFabRamp {
    #[deref]
    draw_super: DrawQuad,
}

/// The layer the view shows on top: the one printing at the playhead, never
/// above the chosen top layer.
pub fn shown_top_layer(p: &PrintState, pv: &PrintPreview) -> u32 {
    // At the end: the top shown (a streaming slice ends with whichever part came last).
    let at = if p.play.time >= pv.total { p.play.layer_hi } else { pv.layers.get(pv.layer_at(p.play.time)).map(|l| l.layer).unwrap_or(0) };
    at.min(p.play.layer_hi).min(pv.max_layer())
}

/// The time span of layer `layer`.
pub fn layer_span(pv: &PrintPreview, layer: u32) -> (f64, f64) {
    pv.layers.iter().find(|l| l.layer == layer).map(|l| (l.t_start, l.t_end)).unwrap_or((0.0, pv.total))
}

// ===========================================================================
// Move slider
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabMoveSlider {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bar: DrawFabRange,
    #[rust]
    dragging: bool,
}

impl FabMoveSlider {
    fn pick(&self, cx: &Cx, abs: DVec2, state: &AppState) -> Option<PrintAction> {
        let pv = state.print.showing().filter(|_| !state.print.making_preview())?;
        let r = self.draw_bar.area().rect(cx);
        let f = ((abs.x - r.pos.x - 7.0) / (r.size.x - 14.0).max(1.0)).clamp(0.0, 1.0);
        let (a, b) = layer_span(pv, shown_top_layer(&state.print, pv));
        Some(PrintAction::Scrub(a + (b - a) * f))
    }
}

impl Widget for FabMoveSlider {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Slider;
        makepad_platform::a11y::set_string(&mut node.name, "Moves in the layer");
        node.range = Some(a11y_tree::RangeValue { min: 0.0, max: 1.0, now: self.draw_bar.head as f64, step: 0.0 });
        node.actions.insert(a11y_tree::Actions::SET_VALUE);
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionArg, ActionResult, Actions};
        if request.action != Actions::SET_VALUE { return ActionResult::Unhandled; }
        let value = match &request.arg {
            ActionArg::Number(value) => Some(*value),
            ActionArg::Text(value) => value.parse::<f64>().ok(),
            _ => None,
        };
        let Some(value) = value.filter(|v| v.is_finite()) else { return ActionResult::Unsupported };
        let Some(state) = scope.data.get::<AppState>() else { return ActionResult::Disabled };
        let Some(pv) = state.print.showing().filter(|_| !state.print.making_preview()) else { return ActionResult::Disabled };
        let (a, b) = layer_span(pv, shown_top_layer(&state.print, pv));
        cx.action(PrintAction::Scrub(a + (b - a) * value.clamp(0.0, 1.0)));
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get::<AppState>() else { return };
        match event.hits(cx, self.draw_bar.area()) {
            Hit::FingerHoverIn(_) => {
                cx.set_cursor(MouseCursor::Hand);
                self.draw_bar.hover = 1.0;
                self.draw_bar.redraw(cx);
            }
            Hit::FingerHoverOut(_) => {
                self.draw_bar.hover = 0.0;
                self.draw_bar.redraw(cx);
            }
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                self.dragging = true;
                if let Some(a) = self.pick(cx, fe.abs, state) {
                    cx.action(a);
                }
            }
            Hit::FingerMove(fe) if self.dragging => {
                if let Some(a) = self.pick(cx, fe.abs, state) {
                    cx.action(a);
                }
            }
            Hit::FingerUp(_) => self.dragging = false,
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let head = scope
            .data
            .get::<AppState>()
            .and_then(|s| s.print.showing().map(|pv| (s, pv)))
            .map(|(s, pv)| {
                let (a, b) = layer_span(pv, shown_top_layer(&s.print, pv));
                ((s.print.play.time - a) / (b - a).max(1e-9)) as f32
            })
            .unwrap_or(0.0);
        self.draw_bar.head = head.clamp(0.0, 1.0);
        self.draw_bar.draw_walk(cx, walk);
        DrawStep::done()
    }
}

// ===========================================================================
// The playback pad
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabSlicePad {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_pad: DrawFabPad,
    #[live]
    draw_mark: DrawFabChip,
    /// What the held press moves: the playhead (the pad) or the floor (its handle on the rail).
    #[rust]
    grab: Option<PadGrab>,
    /// The pause whose mark the pointer is on (its tip shows).
    #[rust]
    tip_pause: Option<usize>,
}

#[derive(Clone, Copy, PartialEq)]
enum PadGrab {
    Head,
    Floor,
}

/// The pad's well inside its rect: the point travels within it.
const PAD_INSET: f64 = 6.0;
/// How near a pause's mark the pointer raises its tip (points).
const PAUSE_REACH: f64 = 4.0;

/// What the pad shows: the top layer (its index of `max`), the point across its moves (none for a
/// resin stack, whose layer cures at once), the floor and the pauses (the layer each comes before,
/// its words). A slice's layers or a resin printer's. O(layers + pauses).
struct PadState {
    max: u32,
    top: u32,
    across: Option<f32>,
    lo: u32,
    pauses: Vec<(u32, String)>,
}

fn pad_state(state: &AppState) -> Option<PadState> {
    let p = &state.print;
    if let Some(r) = p.resin_shown() {
        let max = r.count.saturating_sub(1);
        return Some(PadState { max, top: p.play.layer_hi.min(max), across: None, lo: p.play.layer_lo.min(max), pauses: Vec::new() });
    }
    let pv = p.showing()?;
    let top = shown_top_layer(p, pv);
    let (a, b) = layer_span(pv, top);
    let across = ((p.play.time - a) / (b - a).max(1e-9)) as f32;
    let pauses = pv.pauses.iter().map(|q| (q.layer, q.step())).collect();
    Some(PadState { max: pv.max_layer(), top, across: Some(across.clamp(0.0, 1.0)), lo: p.play.layer_lo, pauses })
}

impl FabSlicePad {
    /// The pad's well (where the point travels) and the rail beside it, in window points.
    fn wells(&self, cx: &Cx) -> (Rect, Rect) {
        let r = self.draw_pad.area().rect(cx);
        let rail = self.draw_pad.rail as f64;
        let well = Rect { pos: r.pos + dvec2(PAD_INSET, PAD_INSET), size: dvec2((r.size.x - rail - PAD_INSET * 2.0).max(1.0), (r.size.y - PAD_INSET * 2.0).max(1.0)) };
        let rail = Rect { pos: dvec2(r.pos.x + r.size.x - rail, r.pos.y), size: dvec2(rail, r.size.y) };
        (well, rail)
    }

    /// The layer at `abs`'s height and the share across the well. O(1).
    fn at(&self, cx: &Cx, abs: DVec2, max: u32) -> (u32, f64) {
        let (well, _) = self.wells(cx);
        let fx = ((abs.x - well.pos.x) / well.size.x).clamp(0.0, 1.0);
        let fy = 1.0 - ((abs.y - well.pos.y) / well.size.y).clamp(0.0, 1.0);
        ((fy * max as f64).round() as u32, fx)
    }

    /// A press or a drag at `abs` with `grab`: the floor to its layer, or that layer on top and
    /// (a slice's) the playhead across its moves.
    fn pick(&self, cx: &mut Cx, abs: DVec2, state: &AppState, grab: PadGrab) {
        let Some(ps) = pad_state(state).filter(|_| !state.print.making_preview()) else { return };
        let (layer, fx) = self.at(cx, abs, ps.max);
        match grab {
            PadGrab::Floor => cx.action(PrintAction::SetBottomLayer(layer.min(ps.top))),
            PadGrab::Head => {
                let layer = layer.max(ps.lo);
                cx.action(PrintAction::SetTopLayer(layer));
                if let (Some(_), Some(pv)) = (ps.across, state.print.showing()) {
                    let (a, b) = layer_span(pv, layer);
                    cx.action(PrintAction::Scrub(a + (b - a) * fx));
                }
            }
        }
    }

    /// The fraction of the way up the well a layer sits at.
    fn up(layer: f32, max: u32) -> f32 {
        (layer / max.max(1) as f32).clamp(0.0, 1.0)
    }
}

/// The pad's parts to a reader: the layer on top and the floor (its handle on the rail), each a
/// slider of its own; the pad itself is the moves across the layer.
const PART_TOP: u64 = 1;
const PART_FLOOR: u64 = 2;

impl Widget for FabSlicePad {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Slider;
        makepad_platform::a11y::set_string(&mut node.name, "Moves in the layer");
        node.range = Some(a11y_tree::RangeValue { min: 0.0, max: 1.0, now: self.draw_pad.px as f64, step: 0.0 });
        node.actions.insert(a11y_tree::Actions::SET_VALUE);
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (key, name, now) in [(PART_TOP, "Top layer", self.draw_pad.py), (PART_FLOOR, "Bottom layer", self.draw_pad.lo)] {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::Slider,
                actions: a11y_tree::Actions::SET_VALUE,
                range: Some(a11y_tree::RangeValue { min: 0.0, max: 1.0, now: now as f64, step: 0.0 }),
                ..Default::default()
            };
            makepad_platform::a11y::set_string(&mut node.name, name);
            out.push((key, None, node));
        }
    }

    fn semantics_part_area(&self, _key: u64) -> Area {
        self.draw_pad.area()
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionArg, ActionResult, Actions};
        if request.action != Actions::SET_VALUE { return ActionResult::Unhandled; }
        let value = match &request.arg {
            ActionArg::Number(value) => Some(*value),
            ActionArg::Text(value) => value.parse::<f64>().ok(),
            _ => None,
        };
        let Some(value) = value.filter(|v| v.is_finite()).map(|v| v.clamp(0.0, 1.0)) else { return ActionResult::Unsupported };
        let Some(state) = scope.data.get::<AppState>() else { return ActionResult::Disabled };
        let Some(ps) = pad_state(state) else { return ActionResult::Disabled };
        let layer = (value * ps.max as f64).round() as u32;
        match cx.a11y.action_part_key(request.node) {
            Some(PART_TOP) => cx.action(PrintAction::SetTopLayer(layer)),
            Some(PART_FLOOR) => cx.action(PrintAction::SetBottomLayer(layer.min(ps.top))),
            Some(_) => return ActionResult::Unhandled,
            None => {
                let Some(pv) = state.print.showing().filter(|_| !state.print.making_preview()) else { return ActionResult::Disabled };
                let (a, b) = layer_span(pv, shown_top_layer(&state.print, pv));
                cx.action(PrintAction::Scrub(a + (b - a) * value));
            }
        }
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let Some(state) = scope.data.get::<AppState>() else { return };
        let on_rail = |cx: &Cx, abs: DVec2| self.wells(cx).1.contains(abs);
        match event.hits(cx, self.draw_pad.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let rail = on_rail(cx, fe.abs);
                cx.set_cursor(if rail { MouseCursor::NsResize } else { MouseCursor::Crosshair });
                let hot = if rail { 1.0 } else { 0.0 };
                if self.draw_pad.hover != 1.0 || self.draw_pad.floor_hot != hot {
                    self.draw_pad.hover = 1.0;
                    self.draw_pad.floor_hot = hot;
                    self.draw_pad.redraw(cx);
                }
                // a pause's mark under the pointer: what goes in there, in its tip
                let (well, _) = self.wells(cx);
                let near = pad_state(state).and_then(|ps| ps.pauses.iter().position(|(layer, _)| {
                    let up = Self::up(*layer as f32 - 0.5, ps.max) as f64;
                    let y = well.pos.y + well.size.y * (1.0 - up);
                    (fe.abs.y - y).abs() <= PAUSE_REACH && fe.abs.x <= well.pos.x + well.size.x
                }).map(|i| (i, ps)));
                let now = near.as_ref().map(|(i, _)| *i);
                if now != self.tip_pause {
                    self.tip_pause = now;
                    match near {
                        Some((i, ps)) => {
                            let up = Self::up(ps.pauses[i].0 as f32 - 0.5, ps.max) as f64;
                            let y = well.pos.y + well.size.y * (1.0 - up);
                            let mut request = TipRequest::plain(ps.pauses[i].1.clone(), Rect { pos: dvec2(well.pos.x, y - 2.0), size: dvec2(well.size.x, 4.0) });
                            request.place = TipPlace::Right;
                            request.wrap_width = 260.0;
                            cx.widget_action(self.uid, TipAction::HoverInWith(request));
                        }
                        None => cx.widget_action(self.uid, TipAction::HoverOut),
                    }
                }
            }
            Hit::FingerHoverOut(_) => {
                self.draw_pad.hover = 0.0;
                self.draw_pad.floor_hot = 0.0;
                self.draw_pad.redraw(cx);
                if self.tip_pause.take().is_some() {
                    cx.widget_action(self.uid, TipAction::HoverOut);
                }
            }
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                let grab = if on_rail(cx, fe.abs) { PadGrab::Floor } else { PadGrab::Head };
                self.grab = Some(grab);
                self.draw_pad.down = if grab == PadGrab::Head { 1.0 } else { 0.0 };
                if self.tip_pause.take().is_some() {
                    cx.widget_action(self.uid, TipAction::HoverOut);
                }
                self.pick(cx, fe.abs, state, grab);
                self.draw_pad.redraw(cx);
            }
            Hit::FingerMove(fe) => {
                if let Some(grab) = self.grab {
                    self.pick(cx, fe.abs, state, grab);
                }
            }
            Hit::FingerUp(_) => {
                self.grab = None;
                self.draw_pad.down = 0.0;
                self.draw_pad.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let ps = scope.data.get::<AppState>().and_then(pad_state);
        let (x, y, lo, resin) = ps.as_ref().map_or((0.0, 0.0, 0.0, 0.0), |ps| {
            (ps.across.unwrap_or(0.5), Self::up(ps.top as f32, ps.max), Self::up(ps.lo as f32, ps.max), if ps.across.is_none() { 1.0 } else { 0.0 })
        });
        self.draw_pad.px = x;
        self.draw_pad.py = y;
        self.draw_pad.lo = lo;
        self.draw_pad.resin = resin;
        let rect = cx.walk_turtle(walk);
        self.draw_pad.draw_abs(cx, rect);
        // the pauses: an amber line across the well at the layer each comes before
        if let Some(ps) = ps.filter(|ps| !ps.pauses.is_empty()) {
            let rail = self.draw_pad.rail as f64;
            let well = Rect { pos: rect.pos + dvec2(PAD_INSET, PAD_INSET), size: dvec2((rect.size.x - rail - PAD_INSET * 2.0).max(1.0), (rect.size.y - PAD_INSET * 2.0).max(1.0)) };
            self.draw_mark.fill = vec4(0.98, 0.70, 0.18, 0.9);
            self.draw_mark.ring = vec4(0.0, 0.0, 0.0, 0.0);
            self.draw_mark.spool = 0.0;
            for (layer, _) in &ps.pauses {
                let up = Self::up(*layer as f32 - 0.5, ps.max) as f64;
                let y = well.pos.y + well.size.y * (1.0 - up);
                self.draw_mark.draw_abs(cx, Rect { pos: dvec2(well.pos.x, y - 1.0), size: dvec2(well.size.x, 2.0) });
            }
        }
        DrawStep::done()
    }
}

// ===========================================================================
// The printer's job along its layers
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabJobTimeline {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bar: DrawFabJobBar,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_dim: DrawText,
    /// What the bar says (its accessible value).
    #[rust]
    text: String,
    #[rust]
    done: f64,
}

impl Widget for FabJobTimeline {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Progress;
        makepad_platform::a11y::set_string(&mut node.name, "Print layers");
        makepad_platform::a11y::set_string(&mut node.value, &self.text);
        node.range = Some(a11y_tree::RangeValue { min: 0.0, max: 1.0, now: self.done, step: 0.0 });
    }

    fn handle_event(&mut self, _cx: &mut Cx, _event: &Event, _scope: &mut Scope) {}

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(job) = scope.data.get::<AppState>().and_then(|s| s.print.follow.shown_printing().cloned()) else {
            self.text.clear();
            cx.walk_turtle(Walk { height: Size::Fixed(0.0), ..walk });
            return DrawStep::done();
        };
        let total = job.total_layers.unwrap_or(job.layer.max(1)).max(1);
        let done = job.layer.saturating_sub(1).min(total) as f64 / total as f64;
        let cur = job.layer.min(total) as f64 / total as f64;
        self.done = done;
        let left = match job.layer {
            0 => format!("Layer 0 / {total}  ·  {}", job.doing()),
            l => format!("Layer {} on the printer  ·  {} to go", job.layer_text(), total.saturating_sub(l)),
        };
        let right = job.progress_text();
        self.text = format!("{left}  ·  {right}");
        let rect = cx.walk_turtle(walk);
        self.draw_text.draw_abs(cx, dvec2(rect.pos.x, rect.pos.y + 2.0), &left);
        let tw = self.draw_dim.prepare_single_line_run(cx, &right).map(|r| r.width_in_lpxs as f64).unwrap_or(80.0);
        self.draw_dim.draw_abs(cx, dvec2(rect.pos.x + rect.size.x - tw, rect.pos.y + 2.0), &right);
        self.draw_bar.done = done as f32;
        self.draw_bar.cur = cur as f32;
        self.draw_bar.layers = total as f32;
        self.draw_bar.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, rect.pos.y + rect.size.y - 9.0), size: dvec2(rect.size.x, 8.0) });
        DrawStep::done()
    }
}

// ===========================================================================
// Legend
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabSliceLegend {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_ramp: DrawFabRamp,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_dim: DrawText,
    #[live]
    draw_head: DrawText,
    #[live]
    draw_warn: DrawText,
    #[live]
    color_text: Vec4f,
    #[live]
    color_dim: Vec4f,
    #[live]
    color_border: Vec4f,
    #[live]
    color_warning: Vec4f,
    #[live]
    color_error: Vec4f,
    #[rust]
    hint: PropHint,
    /// Which of it this one draws: 0 all of it; 1 the colours' key and the print time (the
    /// Legend's first nested section), 2 the materials and the filament (its second). The parts
    /// leave out the key's heading word, their section's title says it.
    #[live]
    part: u32,
    #[rust]
    area: Area,
    /// Material row hit rectangles and their full design-to-spool explanations.
    #[rust]
    hits: Vec<(Rect, String)>,
    /// The legend as text (its accessible value).
    #[rust]
    text: String,
}

/// A material entry's height: its name, and under it the tray and spool it prints from.
const MATERIAL_H: f64 = 32.0;

enum Row {
    Head(String, String),
    Swatch { color: Option<[f32; 4]>, name: String, cols: [String; 3] },
    /// Time that draws no bead (no swatch), what it is made of under its name.
    Change { name: String, detail: String, cols: [String; 3] },
    Material { color: Option<[f32; 4]>, name: String, detail: String, cols: [String; 3], mismatch: bool, unassigned: bool, tip: String },
    Ramp(String, String),
    Line(String, String),
    Gap,
}

/// A job tool's colours and the loaded spool, with the same mapping text as the colour rows.
fn material_row(mode: fab_plate::ColourMode, p: &PrintState, tool: usize, color: Option<[f32; 4]>, cols: [String; 3]) -> Row {
    let pos = if p.monitor_holds() { None } else { p.colors.tools.get(tool) };
    let mapped: Vec<_> = p.colors.rows.iter().filter(|r| r.used && !r.unassigned() && r.pos.as_ref() == pos).collect();
    let names = mapped.iter().map(|r| r.name.as_str()).collect::<Vec<_>>().join(", ");
    let first = mapped.first();
    let (name, detail) = match first {
        Some(row) => (names, format!("→ {}", mapping_text(row, &p.colors.layout.heads))),
        None => (format!("Material {}", tool + 1), String::new()),
    };
    let mismatch = first.is_some_and(|row| row.mismatch.is_some() && color.is_some_and(|[r, g, b, _]| dist(row.design, [r, g, b]) > VISIBLE_DIFF));
    let tip = if let Some(row) = first {
        let designs = mapped.iter().map(|r| format!("{} (design {})", r.name, hex(r.design))).collect::<Vec<_>>().join(", ");
        let mut what = Vec::new();
        let kind = row.type_label();
        if !kind.is_empty() && kind != "?" {
            what.push(kind);
        }
        if let Some([r, g, b, _]) = row.spool().filter(|_| p.target_printer().is_some()) {
            what.push(format!("{} spool", colour_word([r, g, b])));
        } else {
            what.push("spool colour not reported".into());
        }
        let place = row.pos.map(|pos| pos.label(&p.colors.layout.heads)).unwrap_or_default();
        let view = missing_spool_text(mode);
        format!("{designs} prints on tray {place}: {}. {view}", what.join(", "))
    } else {
        color.map(|[r, g, b, _]| format!("{name}: drawn in {} ({}).", colour_word([r, g, b]), hex([r, g, b]))).unwrap_or_else(|| name.clone())
    };
    Row::Material { color, name, detail, cols, mismatch, unassigned: false, tip }
}

fn missing_spool_text(mode: fab_plate::ColourMode) -> &'static str {
    match mode {
        fab_plate::ColourMode::Model => "A tool with no spool colour shows its model colour.",
        fab_plate::ColourMode::Material => "A tool with no spool colour shows red.",
    }
}

fn unassigned_rows(mode: fab_plate::ColourMode, p: &PrintState, out: &mut Vec<Row>) {
    for row in p.colors.rows.iter().filter(|r| r.used && r.unassigned()) {
        let [r, g, b] = match mode {
            fab_plate::ColourMode::Model => row.design,
            fab_plate::ColourMode::Material => fab_plate::UNMAPPED_RGB,
        };
        out.push(Row::Material { color: Some([r, g, b, 1.0]), name: row.name.clone(), detail: format!("No tray · {}", missing_spool_text(mode)), cols: Default::default(), mismatch: false, unassigned: true, tip: format!("{UNASSIGNED_TIP} {}", missing_spool_text(mode)) });
    }
}

fn palette_caption(mode: fab_plate::ColourMode) -> Row {
    Row::Line(match mode {
        fab_plate::ColourMode::Model => "Drawn in the model colours",
        fab_plate::ColourMode::Material => "Drawn in the loaded spools' colours",
    }.into(), String::new())
}

fn metres(mm: f64) -> String {
    if mm >= 1000.0 {
        format!("{:.2} m", mm / 1000.0)
    } else {
        format!("{mm:.0} mm")
    }
}

/// The legend's rows, or one part of them (see [`FabSliceLegend::part`]).
fn legend_rows(s: &AppState, part: u32) -> Vec<Row> {
    let mut out = all_legend_rows(s);
    if part == 0 {
        // whole, the materials' heading only over their table
        out.retain(|r| !matches!(r, Row::Head(name, cols) if name == "MATERIAL" && cols.is_empty()));
        return out;
    }
    let split = out.iter().position(|r| matches!(r, Row::Head(_, _)) && material_head(r)).unwrap_or(out.len());
    let mut rows: Vec<Row> = if part == 1 { out.drain(..split).collect() } else { out.drain(split..).collect() };
    // the heading word is the section's title: a heading keeps its columns' names, or goes
    rows.retain(|r| !matches!(r, Row::Head(_, cols) if cols.is_empty()));
    for r in rows.iter_mut() {
        if let Row::Head(name, _) = r {
            name.clear();
        }
    }
    while matches!(rows.first(), Some(Row::Gap)) {
        rows.remove(0);
    }
    rows
}

/// Where the materials begin: their table's heading, or the totals' first material line.
fn material_head(r: &Row) -> bool {
    matches!(r, Row::Head(name, _) if name == "MATERIAL")
}

/// The Legend's Material section has rows: a preview with its materials (not the overhang key).
pub fn legend_has_materials(s: &AppState) -> bool {
    !legend_rows(s, 2).is_empty()
}

fn all_legend_rows(s: &AppState) -> Vec<Row> {
    let p = &s.print;
    let mut out = Vec::new();
    if p.overhangs && !p.play.active {
        out.push(Row::Head("OVERHANG".into(), String::new()));
        for (d, c) in fab_printability::OVERHANG_COLORS.iter().enumerate() {
            out.push(Row::Swatch { color: Some(*c), name: if d == 0 { "Supported".into() } else { format!("Degree {d}") }, cols: Default::default() });
        }
        return out;
    }
    let Some(pv) = p.showing() else { return out };
    // The held job has its own tool order and can predate the model palette.
    let held = p.monitor_holds();
    let mode = if held && pv.colours.model.is_empty() { fab_plate::ColourMode::Material } else { s.view.colours };
    let palette = pv.colours.shown(mode);
    match p.play.color {
        ColorBy::Feature => {
            out.push(Row::Head("FEATURE".into(), "TIME      SHARE     LENGTH".into()));
            let total: f64 = pv.roles.iter().map(|r| r.seconds).chain(pv.changes.iter().map(|c| c.seconds)).sum::<f64>().max(1e-9);
            for r in &pv.roles {
                out.push(Row::Swatch { color: Some(r.color), name: r.name.clone(), cols: [format_time(r.seconds), format!("{:.0}%", 100.0 * r.seconds / total), metres(r.length_mm)] });
            }
            for c in &pv.changes {
                out.push(Row::Change { name: c.name.clone(), detail: c.detail.clone(), cols: [format_time(c.seconds), format!("{:.0}%", 100.0 * c.seconds / total), String::new()] });
            }
        }
        ColorBy::Speed => {
            out.push(Row::Head("SPEED".into(), String::new()));
            out.push(Row::Ramp(format!("{:.0} mm/s", pv.speed_range[0]), format!("{:.0} mm/s", pv.speed_range[1])));
        }
        ColorBy::Layer => {
            out.push(Row::Head("LAYER".into(), String::new()));
            out.push(Row::Ramp("1".into(), format!("{}", pv.max_layer() + 1)));
        }
        ColorBy::Nozzle => {
            out.push(Row::Head("NOZZLE".into(), String::new()));
            out.push(Row::Swatch { color: Some(NOZZLE_COLORS[0]), name: "Left".into(), cols: Default::default() });
            out.push(Row::Swatch { color: Some(NOZZLE_COLORS[1]), name: "Right".into(), cols: Default::default() });
        }
        ColorBy::Filament => {
            out.push(Row::Head("FILAMENT".into(), String::new()));
            for (i, c) in palette.iter().enumerate() {
                out.push(material_row(mode, p, pv.job_tool(i), Some(*c), Default::default()));
            }
            if !held { unassigned_rows(mode, p, &mut out); }
            out.push(palette_caption(mode));
        }
        ColorBy::Mode => {
            out.push(Row::Head("MODE".into(), "TIME      LENGTH    MAX Z".into()));
            for m in &pv.modes {
                let color = MODE_COLORS.get(m.mode as usize).copied().unwrap_or(MODE_COLORS[0]);
                let name = MODE_NAMES.get(m.mode as usize).copied().unwrap_or("Standard").to_string();
                let z = if m.max_z_rate > 0.0 { format!("{:.1} mm/s", m.max_z_rate) } else { String::new() };
                out.push(Row::Swatch { color: Some(color), name, cols: [format_time(m.time_s), metres(m.filament_mm), z] });
            }
        }
    }
    // A non-planar top: its summary, and the planar terraces it replaces when compared.
    if let Some(np) = &pv.nonplanar {
        if p.play.compare_planar {
            out.push(Row::Swatch { color: Some(GHOST_COLOR), name: "Planar terraces (before)".into(), cols: Default::default() });
        }
        for (k, v) in np.rows() {
            out.push(Row::Line(k.to_string(), v.to_string()));
        }
    }
    if p.play.travel {
        out.push(Row::Swatch { color: Some(TRAVEL_COLOR), name: "Travel".into(), cols: Default::default() });
        out.push(Row::Swatch { color: Some(WIPE_COLOR), name: "Wipe".into(), cols: Default::default() });
    }
    // Totals. The live slice's tiles carry no times: its estimate (the planner's, after the
    // tiles) brings them, until then they say so rather than show nothing as zero.
    out.push(Row::Gap);
    let timed = pv.total > 0.0 || !pv.filament_use.is_empty();
    // An estimate ahead of its playing preview says its totals meanwhile.
    let ahead = p.estimate.filter(|_| !timed);
    let (seconds, prepare) = ahead.map_or((pv.total, pv.prepare_seconds), |e| (e.seconds, e.prepare_seconds));
    out.push(Row::Line("Print time".into(), if timed || ahead.is_some() { format_time(seconds) } else { "estimating…".into() }));
    if prepare > 0.0 {
        out.push(Row::Line("  of which printer routines".into(), format_time(prepare)));
    }
    let plate = pv.printability.plate.as_ref();
    let cost_of = |slot: usize| plate.and_then(|p| p.filaments.iter().find(|f| f.filament as usize == slot + 1)).map(|f| f.cost).filter(|c| *c > 0.0);
    // Each material's print, then what the changes purge of them all, on its own row.
    // (always a heading: it is where the Legend's Material section begins)
    out.push(Row::Head("MATERIAL".into(), if pv.filament_use.is_empty() { String::new() } else { "PRINTS    TIME      COST".into() }));
    let (mut purge_grams, mut purge_cost) = (0.0, None::<f64>);
    for u in &pv.filament_use {
        let color = palette.get(u.slot).copied();
        let printed = (u.grams - u.purge_grams).max(0.0);
        let share = if u.grams > 0.0 { printed / u.grams } else { 1.0 };
        let cost = cost_of(u.slot);
        if let Some(c) = cost {
            *purge_cost.get_or_insert(0.0) += c * (1.0 - share);
        }
        purge_grams += u.purge_grams;
        let mut row = material_row(mode, p, pv.job_tool(u.slot), color, [format!("{printed:.1} g"), format_time(u.seconds), cost.map(|c| format!("{:.2}", c * share)).unwrap_or_default()]);
        if let Row::Material { tip, .. } = &mut row {
            tip.push_str(&format!(" It feeds {:.1} g ({:.2} m) of filament", u.grams, u.metres));
            tip.push_str(&if u.purge_grams > 0.0 { format!(", {:.1} g of it purged at its changes.", u.purge_grams) } else { ".".into() });
        }
        out.push(row);
    }
    if pv.purges > 0 {
        let grams: f64 = pv.filament_use.iter().map(|u| u.grams).sum::<f64>().max(1e-9);
        out.push(Row::Change {
            name: "Purge".into(),
            detail: format!("{} filament swap{} · {:.0}% of the filament", pv.purges, if pv.purges == 1 { "" } else { "s" }, 100.0 * purge_grams / grams),
            cols: [format!("{purge_grams:.1} g"), format_time(pv.purge_seconds), purge_cost.map(|c| format!("{c:.2}")).unwrap_or_default()],
        });
    }
    if !held && p.play.color != ColorBy::Filament { unassigned_rows(mode, p, &mut out); }
    if !pv.filament_use.is_empty() && p.play.color != ColorBy::Filament { out.push(palette_caption(mode)); }
    // (from +0: an empty sum of floats is -0, which prints "-0.0")
    let grams = pv.filament_use.iter().fold(0.0, |a, u| a + u.grams);
    let mut total = match ahead {
        _ if timed => format!("{grams:.1} g"),
        Some(e) => format!("{:.1} g", e.grams),
        None => "estimating…".into(),
    };
    if let Some(cost) = plate.map(|p| p.cost).filter(|c| *c > 0.0) {
        total.push_str(&format!(" · cost {cost:.2}"));
    }
    out.push(Row::Line("Filament".into(), total));
    out
}

impl Widget for FabSliceLegend {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::StaticText;
        makepad_platform::a11y::set_string(&mut node.name, "Slice legend");
        makepad_platform::a11y::set_string(&mut node.value, &self.text);
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if let Event::MouseMove(e) = event {
            let over = self.hits.iter().enumerate().find(|(_, (r, _))| r.contains(e.abs));
            match over {
                Some((i, (rect, text))) => self.hint.hover(cx, self.uid, Some((LiveId(i as u64 + 1), *rect, text.as_str()))),
                None => self.hint.clear(cx, self.uid),
            }
        }
        match event.hits(cx, self.area) {
            Hit::FingerDown(_) | Hit::FingerHoverOut(_) => self.hint.clear(cx, self.uid),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let rows = scope.data.get::<AppState>().map(|s| legend_rows(s, self.part)).unwrap_or_default();
        self.text.clear();
        self.hits.clear();
        for r in &rows {
            let line = match r {
                Row::Head(a, _) => a.clone(),
                Row::Swatch { name, cols, .. } => format!("{name} {}", cols.iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join(" ")),
                Row::Material { name, detail, cols, tip, .. } => format!("{name} {detail}. {tip} {}", cols.iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join(" ")),
                Row::Change { name, detail, cols } => format!("{name} {detail} {}", cols.iter().filter(|c| !c.is_empty()).cloned().collect::<Vec<_>>().join(" ")),
                Row::Ramp(lo, hi) => format!("{lo} to {hi}"),
                Row::Line(a, b) => format!("{a} {b}"),
                Row::Gap => continue,
            };
            self.text.push_str(line.trim());
            self.text.push('\n');
        }
        let row_h = 18.0;
        let h: f64 = rows.iter().map(|r| match r { Row::Gap => 8.0, Row::Ramp(..) => 30.0, Row::Material { .. } | Row::Change { .. } => MATERIAL_H, _ => row_h }).sum();
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(h), ..walk });
        let right = rect.pos.x + rect.size.x;
        let col_x = [right - 128.0, right - 80.0, right - 42.0];
        let mut y = rect.pos.y;
        for row in &rows {
            self.draw_text.color = self.color_text;
            match row {
                Row::Head(a, b) => {
                    self.draw_head.draw_abs(cx, dvec2(rect.pos.x, y + 3.0), a);
                    if !b.is_empty() {
                        for (i, w) in b.split_whitespace().enumerate().take(3) {
                            self.draw_head.draw_abs(cx, dvec2(col_x[i], y + 3.0), w);
                        }
                    }
                    y += row_h;
                }
                Row::Swatch { color, name, cols } | Row::Material { color, name, cols, .. } => {
                    let (mismatch, unassigned, detail) = match row {
                        Row::Material { mismatch, unassigned, tip, detail, .. } => {
                            self.hits.push((Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, MATERIAL_H) }, tip.clone()));
                            (*mismatch, *unassigned, detail.as_str())
                        }
                        _ => (false, false, ""),
                    };
                    self.draw_chip.spool = 0.0;
                    self.draw_chip.empty = 0.0;
                    self.draw_chip.radius = 1.5;
                    self.draw_chip.ring_width = if unassigned { 1.5 } else { 1.0 };
                    self.draw_chip.fill = color.map(|[r, g, b, _]| {
                        let [r, g, b] = if matches!(row, Row::Material { .. }) { crate::ui::popover::display_rgb([r, g, b]) } else { [r, g, b] };
                        vec4(r, g, b, 1.0)
                    }).unwrap_or(vec4(0.0, 0.0, 0.0, 0.0));
                    self.draw_chip.ring = if unassigned { self.color_error } else { self.color_border };
                    let swatch = Rect { pos: dvec2(rect.pos.x, y + 4.0), size: dvec2(10.0, 10.0) };
                    self.draw_chip.draw_abs(cx, swatch);
                    if mismatch {
                        let mark = Rect { pos: swatch.pos + dvec2(5.0, -4.0), size: dvec2(10.0, 10.0) };
                        warning_mark(cx, &mut self.draw_chip, &mut self.draw_warn, mark, self.color_warning, "≠");
                    }
                    self.draw_text.color = if unassigned { self.color_error } else { self.color_text };
                    let name_x = rect.pos.x + 20.0;
                    let end = cols.iter().position(|c| !c.is_empty()).map_or(right, |i| col_x[i] - 6.0);
                    let name = ellipsis(cx, &self.draw_text, name, (end - name_x).max(0.0));
                    self.draw_text.draw_abs(cx, dvec2(name_x, y + 3.0), &name);
                    for (i, c) in cols.iter().enumerate() {
                        if !c.is_empty() {
                            self.draw_dim.draw_abs(cx, dvec2(col_x[i], y + 3.0), c);
                        }
                    }
                    if detail.is_empty() {
                        y += row_h;
                    } else {
                        // The tray and spool this tool prints from, under its name.
                        self.draw_dim.color = if unassigned { self.color_error } else { self.color_dim };
                        let detail = ellipsis(cx, &self.draw_dim, detail, (right - name_x).max(0.0));
                        self.draw_dim.draw_abs(cx, dvec2(name_x, y + 17.0), &detail);
                        self.draw_dim.color = self.color_dim;
                        y += MATERIAL_H;
                    }
                }
                Row::Change { name, detail, cols } => {
                    let name_x = rect.pos.x + 20.0;
                    let name = ellipsis(cx, &self.draw_text, name, (col_x[0] - 6.0 - name_x).max(0.0));
                    self.draw_text.draw_abs(cx, dvec2(name_x, y + 3.0), &name);
                    for (i, c) in cols.iter().enumerate() {
                        if !c.is_empty() {
                            self.draw_dim.draw_abs(cx, dvec2(col_x[i], y + 3.0), c);
                        }
                    }
                    let detail = ellipsis(cx, &self.draw_dim, detail, (right - name_x).max(0.0));
                    self.draw_dim.draw_abs(cx, dvec2(name_x, y + 17.0), &detail);
                    y += MATERIAL_H;
                }
                Row::Ramp(lo, hi) => {
                    self.draw_ramp.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y + 3.0), size: dvec2(rect.size.x, 9.0) });
                    self.draw_dim.draw_abs(cx, dvec2(rect.pos.x, y + 15.0), lo);
                    let tw = self.draw_dim.prepare_single_line_run(cx, hi).map(|r| r.width_in_lpxs as f64).unwrap_or(30.0);
                    self.draw_dim.draw_abs(cx, dvec2(right - tw, y + 15.0), hi);
                    y += 30.0;
                }
                Row::Line(a, b) => {
                    let tw = self.draw_text.prepare_single_line_run(cx, b).map(|r| r.width_in_lpxs as f64).unwrap_or(0.0);
                    let room = rect.size.x - if b.is_empty() { 0.0 } else { tw + 6.0 };
                    let a = ellipsis(cx, &self.draw_dim, a, room.max(0.0));
                    self.draw_dim.draw_abs(cx, dvec2(rect.pos.x, y + 3.0), &a);
                    self.draw_text.draw_abs(cx, dvec2(right - tw, y + 3.0), b);
                    y += row_h;
                }
                Row::Gap => {
                    self.draw_chip.spool = 0.0;
                    self.draw_chip.radius = 0.0;
                    self.draw_chip.ring_width = 1.0;
                    self.draw_chip.fill = self.color_border;
                    self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                    self.draw_chip.draw_abs(cx, Rect { pos: dvec2(rect.pos.x, y + 3.5), size: dvec2(rect.size.x, 1.0) });
                    y += 8.0;
                }
            }
        }
        cx.add_rect_area(&mut self.area, rect);
        DrawStep::done()
    }
}

// ===========================================================================
// Printability summary
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabIssueSummary {
    #[uid]
    uid: WidgetUid,
    /// In a section titled for it: no PRINTABILITY heading row, only a "Checking n/m" line while
    /// the checks run.
    #[live]
    headless: bool,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_chip: DrawFabChip,
    #[live]
    draw_hit: DrawFabChip,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_dim: DrawText,
    #[live]
    draw_head: DrawText,
    #[live]
    draw_prev: DrawSvg,
    #[live]
    draw_next: DrawSvg,
    #[rust]
    area: Area,
    /// Each line as drawn.
    #[rust]
    lines: Vec<IssueLine>,
    #[rust]
    hover: Option<(usize, RowPart)>,
    #[rust]
    text: String,
}

/// A line as drawn: its rect, its step buttons when its kind has more than one issue, its issues
/// in step order (none: the trapped supports, which open the support check), its tip and what a
/// screen reader says of it.
struct IssueLine {
    rect: Rect,
    steps: Option<(Rect, Rect)>,
    of: Vec<usize>,
    name: String,
    tip: String,
    label: String,
}

/// One kind of issue: its name, worst severity, how many, and its issues in step order.
struct IssueKind {
    name: String,
    severity: fab_printability::Severity,
    count: usize,
    of: Vec<usize>,
}

/// The report's kinds of issue as the list shows them, each with its issues in the order its
/// row steps through them: the worst severity first, then as the report lists them.
/// Whether the issue summary shows anything: something sliced, or something found (the Slice
/// editor hides its Printability section otherwise).
pub(crate) fn issues_shown(p: &crate::print::PrintState) -> bool {
    p.showing().is_some()
        || !issue_kinds(&p.report()).is_empty()
        || p.support_pieces.iter().flatten().any(|piece| piece.light == crate::api::MechLight::Red)
}

fn issue_kinds(report: &fab_printability::Report) -> Vec<IssueKind> {
    let mut out: Vec<(&'static str, IssueKind)> = Vec::new();
    for (i, issue) in report.issues.iter().enumerate() {
        match out.iter_mut().find(|(k, _)| *k == issue.kind) {
            Some((_, g)) => {
                g.count += 1;
                g.of.push(i);
            }
            None => {
                let mut name = issue.kind.replace('_', " ");
                if let Some(c) = name.get_mut(0..1) {
                    c.make_ascii_uppercase();
                }
                out.push((issue.kind, IssueKind { name, severity: issue.severity, count: 1, of: vec![i] }));
            }
        }
    }
    for (_, g) in &mut out {
        g.of.sort_by_key(|&i| report.issues[i].severity as u8);
        g.severity = report.issues[g.of[0]].severity;
    }
    out.into_iter().map(|(_, g)| g).collect()
}

fn issue_tip(issue: &fab_printability::Issue) -> String {
    let what = issue.object.as_deref().unwrap_or("plate");
    let layer = issue.layer.map(|l| format!(" · layer {}", l + 1)).unwrap_or_default();
    format!("{} — {what}{layer}. {}", issue.message, issue.hint)
}

/// The issue a press on `part` of a line with issues `of` shows, `picked` shown last: the row
/// shows the one it shows (its first, until one of it is picked), the buttons step from it.
fn issue_pressed(of: &[usize], picked: Option<usize>, part: RowPart) -> Option<usize> {
    let at = picked.and_then(|p| of.iter().position(|&i| i == p));
    let to = match part {
        RowPart::Row => (!of.is_empty()).then(|| at.unwrap_or(0)),
        RowPart::Prev => stepper::step(of.len(), at, false),
        RowPart::Next => stepper::step(of.len(), at, true),
    };
    to.map(|to| of[to])
}

const ISSUE_ROW_H: f64 = 18.0;

impl FabIssueSummary {
    fn line_at(&self, abs: DVec2) -> Option<(usize, RowPart)> {
        let n = self.lines.iter().position(|l| l.rect.contains(abs))?;
        Some((n, stepper::part_at(self.lines[n].steps, abs)))
    }

    fn press(&mut self, cx: &mut Cx, scope: &mut Scope, line: usize, part: RowPart) {
        // the arrow keys step on from here
        cx.set_key_focus(self.area);
        let Some(l) = self.lines.get(line) else { return };
        if l.of.is_empty() {
            cx.action(PrintAction::OpenSupportCheck);
            return;
        }
        let picked = scope.data.get::<AppState>().and_then(|s| s.print.picked_issue);
        if let Some(i) = issue_pressed(&l.of, picked, part) {
            cx.action(PrintAction::SelectIssue(i));
        }
    }

    /// The line showing the picked issue: the one the arrow keys step.
    fn picked_line(&self, scope: &mut Scope) -> Option<usize> {
        let picked = scope.data.get::<AppState>().and_then(|s| s.print.picked_issue)?;
        self.lines.iter().position(|l| l.of.contains(&picked))
    }

    fn hover_tip(&self, cx: &mut Cx, now: Option<(usize, RowPart)>) {
        match now {
            Some((n, part)) => {
                let l = &self.lines[n];
                let (rect, tip) = match (part, l.steps) {
                    (RowPart::Prev, Some((prev, _))) => (prev, format!("The previous {}. The arrow keys step too.", l.name.to_lowercase())),
                    (RowPart::Next, Some((_, next))) => (next, format!("The next {}. The arrow keys step too.", l.name.to_lowercase())),
                    _ => (l.rect, l.tip.clone()),
                };
                let mut request = TipRequest::plain(tip, rect);
                request.place = TipPlace::Right;
                request.wrap_width = 280.0;
                cx.widget_action(self.uid, TipAction::HoverInWith(request));
            }
            None => cx.widget_action(self.uid, TipAction::HoverOut),
        }
    }
}

impl Widget for FabIssueSummary {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::StaticText;
        makepad_platform::a11y::set_string(&mut node.name, "Printability");
        makepad_platform::a11y::set_string(&mut node.value, &self.text);
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (n, l) in self.lines.iter().enumerate() {
            let key = n as u64 * 3;
            let mut parts = vec![(key + 1, l.rect, l.label.clone())];
            if let Some((prev, next)) = l.steps {
                parts.push((key + 2, prev, format!("Previous {}", l.name.to_lowercase())));
                parts.push((key + 3, next, format!("Next {}", l.name.to_lowercase())));
            }
            for (key, rect, label) in parts {
                let mut node = a11y_tree::Node {
                    role: a11y_tree::Role::Button,
                    bounds: makepad_platform::a11y::semantic_rect(rect),
                    actions: a11y_tree::Actions::PRESS,
                    ..Default::default()
                };
                makepad_platform::a11y::set_string(&mut node.name, &label);
                out.push((key, None, node));
            }
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if request.action != Actions::PRESS {
            return ActionResult::Unhandled;
        }
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(key) = key.checked_sub(1) else { return ActionResult::Unsupported };
        let (line, part) = (key as usize / 3, [RowPart::Row, RowPart::Prev, RowPart::Next][key as usize % 3]);
        if line >= self.lines.len() {
            return ActionResult::Unsupported;
        }
        self.press(cx, scope, line, part);
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        match event.hits(cx, self.area) {
            Hit::FingerDown(_) => cx.set_key_focus(self.area),
            Hit::FingerUp(fe) if !fe.cancelled => {
                if let Some((n, part)) = self.line_at(fe.abs) {
                    self.press(cx, scope, n, part);
                }
            }
            Hit::KeyDown(ke) => {
                if let (Some(forward), Some(n)) = (stepper::key_step(ke.key_code), self.picked_line(scope)) {
                    self.press(cx, scope, n, if forward { RowPart::Next } else { RowPart::Prev });
                }
            }
            Hit::FingerHoverIn(fh) | Hit::FingerHoverOver(fh) => {
                let now = self.line_at(fh.abs);
                if now != self.hover {
                    self.hover = now;
                    self.hover_tip(cx, now);
                    self.area.redraw(cx);
                }
                cx.set_cursor(if now.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                cx.widget_action(self.uid, TipAction::HoverOut);
                self.hover = None;
                self.area.redraw(cx);
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        use fab_printability::Severity;
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let p = &state.print;
        let report = p.report().clone();
        let checking = p.streamed.as_ref().and_then(|s| s.checking()).filter(|_| !p.stale);
        let mut kinds = issue_kinds(&report);
        let trapped: Vec<_> = p.support_pieces.iter().flatten().filter(|p| p.light == crate::api::MechLight::Red).map(|p| p.label.as_str()).collect();
        let trapped_tip = format!("{}: no way out or no tool reach found. Mechanics, Supports, shows each one. You can still print.", trapped.join(", "));
        if !trapped.is_empty() {
            kinds.push(IssueKind { name: "Trapped supports".to_string(), severity: Severity::Warning, count: trapped.len(), of: Vec::new() });
        }
        // Nothing sliced and nothing found: the section has nothing to say.
        let shown = !kinds.is_empty() || p.showing().is_some();
        let head_row = !self.headless || checking.is_some();
        let rows = if shown { head_row as usize + kinds.len().max(1) } else { 0 };
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(rows as f64 * ISSUE_ROW_H), ..walk });
        self.lines.clear();
        self.text.clear();
        if !shown {
            self.area = Area::Empty;
            return DrawStep::done();
        }
        self.draw_hit.fill = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_hit.ring = vec4(0.0, 0.0, 0.0, 0.0);
        self.draw_hit.draw_abs(cx, rect);
        self.area = self.draw_hit.area();
        if head_row {
            let head = match (self.headless, checking) {
                (true, Some((done, total))) => format!("Checking {done}/{total}"),
                (_, Some((done, total))) => format!("PRINTABILITY · checking {done}/{total}"),
                (_, None) => "PRINTABILITY".to_string(),
            };
            self.draw_head.draw_abs(cx, dvec2(rect.pos.x, rect.pos.y + 3.0), &head);
        }
        let right = rect.pos.x + rect.size.x;
        let mut y = rect.pos.y + if head_row { ISSUE_ROW_H } else { 0.0 };
        if kinds.is_empty() {
            self.draw_dim.draw_abs(cx, dvec2(rect.pos.x + 16.0, y + 3.0), "No issues");
            self.text.push_str("No issues");
        }
        for (n, k) in kinds.iter().enumerate() {
            let line = Rect { pos: dvec2(rect.pos.x, y), size: dvec2(rect.size.x, ISSUE_ROW_H) };
            // The issue this row shows: the picked one when it is of this kind, else its first.
            let at = p.picked_issue.and_then(|i| k.of.iter().position(|&o| o == i));
            let hover = self.hover.filter(|(h, _)| *h == n).map(|(_, part)| part);
            if at.is_some() || hover == Some(RowPart::Row) {
                self.draw_chip.fill = vec4(1.0, 1.0, 1.0, if at.is_some() { 0.12 } else { 0.07 });
                self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_chip.draw_abs(cx, line);
            }
            let c = match k.severity {
                Severity::Error => vec4(0.878, 0.251, 0.251, 1.0),
                Severity::Warning => vec4(0.878, 0.627, 0.125, 1.0),
                Severity::Info => vec4(0.35, 0.6, 0.95, 1.0),
            };
            self.draw_chip.spool = 0.0;
            self.draw_chip.fill = c;
            self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.4);
            self.draw_chip.draw_abs(cx, Rect { pos: dvec2(rect.pos.x + 2.0, y + 4.0), size: dvec2(10.0, 10.0) });
            let count = stepper::count_text(k.count, at);
            let cw = self.draw_dim.prepare_single_line_run(cx, &count).map(|r| r.width_in_lpxs as f64).unwrap_or(10.0);
            // More than one of a kind: step through them with the buttons beside the count.
            let steps = (k.of.len() > 1).then(|| stepper::buttons(line, cw));
            let name_end = match steps {
                Some((prev, next)) => {
                    for (button, part) in [(prev, RowPart::Prev), (next, RowPart::Next)] {
                        if hover == Some(part) {
                            self.draw_chip.fill = vec4(1.0, 1.0, 1.0, 0.14);
                            self.draw_chip.ring = vec4(0.0, 0.0, 0.0, 0.0);
                            self.draw_chip.draw_abs(cx, button);
                        }
                    }
                    self.draw_prev.draw_abs(cx, stepper::icon(prev));
                    self.draw_dim.draw_abs(cx, dvec2(stepper::count_x(prev), y + 3.0), &count);
                    self.draw_next.draw_abs(cx, stepper::icon(next));
                    prev.pos.x
                }
                None => {
                    self.draw_dim.draw_abs(cx, dvec2(right - cw - 2.0, y + 3.0), &count);
                    right - cw - 2.0
                }
            };
            let name_x = rect.pos.x + 18.0;
            let name = ellipsis(cx, &self.draw_text, &k.name, (name_end - 6.0 - name_x).max(0.0));
            self.draw_text.draw_abs(cx, dvec2(name_x, y + 3.0), &name);
            let shows = at.map_or(0, |a| a + 1);
            let label = match at {
                Some(_) => format!("{}, {shows} of {} ({})", k.name, k.count, k.severity.name()),
                None => format!("{}, {} ({})", k.name, k.count, k.severity.name()),
            };
            self.text.push_str(&label);
            self.text.push('\n');
            let tip = match k.of.get(at.unwrap_or(0)) {
                Some(&i) => issue_tip(&report.issues[i]),
                None => trapped_tip.clone(),
            };
            self.lines.push(IssueLine { rect: line, steps, of: k.of.clone(), name: k.name.clone(), tip, label });
            y += ISSUE_ROW_H;
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fab_printability::{Footprint, Issue, Report, Severity};

    fn issue(kind: &'static str, severity: Severity, layer: u32) -> Issue {
        let at = [10.0 + layer as f64, 20.0, 0.2 * (layer + 1) as f64];
        Issue {
            kind, severity, object: Some("part".into()), at_mm: at, footprint: Footprint::square(at, 2.0), layer: Some(layer), layer_to: None,
            measure: 0.0, message: format!("{kind} at layer {layer}"), hint: String::new(), fix: None, area_mm2: 1.0, lowest_z: at[2], highest_z: at[2], flags: 0,
        }
    }

    /// The Printability list's "Mid air 8" row steps through all eight of them and wraps round,
    /// each step showing its issue at its layer, framed; back from the first goes to the last.
    #[test]
    fn a_kind_of_eight_steps_through_all_eight_and_wraps() {
        let mut issues: Vec<Issue> = (0..8).map(|l| issue("mid_air", Severity::Warning, l)).collect();
        // another kind in between, and one of the eight worse than the rest: it comes first
        issues.insert(3, issue("thin_wall", Severity::Info, 2));
        issues[6].severity = Severity::Error;
        // a mesh check's issue has no layer: it shows at the layer its height prints on
        issues[3].layer = None;
        issues[3].at_mm[2] = 0.5;
        // a live slice's issues, as they stream in beside its preview
        let mut state = PrintState::default();
        state.stream_preview(crate::print::follow::tests::preview_of(8));
        let markers = RetainedInstances::new(PREVIEW_SLOTS, Vec::<f32>::new().into()).unwrap();
        state.live_issues(&Arc::new(Report { issues, ..Default::default() }), &markers);
        let kinds = issue_kinds(state.report());
        assert_eq!(kinds.iter().map(|k| (k.name.as_str(), k.count)).collect::<Vec<_>>(), [("Mid air", 8), ("Thin wall", 1)]);
        let mid_air = &kinds[0].of;
        assert_eq!(mid_air[0], 6, "the worst of them first");
        assert_eq!(kinds[0].severity, Severity::Error);

        // the row shows its first; then Next goes through all eight and comes back to it
        let mut seen = Vec::new();
        let pick = |state: &mut PrintState, part: RowPart| {
            let i = issue_pressed(mid_air, state.picked_issue, part).expect("an issue to show");
            assert!(apply(state, &PrintAction::SelectIssue(i)));
            let at = mid_air.iter().position(|&o| o == i).unwrap();
            let shown = &state.report().issues[i];
            assert_eq!(state.play.layer_hi, shown.layer.unwrap(), "the layer slider goes to its layer");
            assert!(state.issue_spot.is_some(), "it is framed");
            assert_eq!(stepper::count_text(mid_air.len(), Some(at)), format!("{} / 8", at + 1));
            i
        };
        // an overhang shown before: the issue is what shows now
        state.picked_overhang = Some(crate::print::support_list::Picked { line: 0, min: [0.0; 3], max: [1.0; 3], member: None });
        let first = pick(&mut state, RowPart::Row);
        assert_eq!(first, mid_air[0]);
        assert_eq!(state.picked_overhang, None);
        seen.push(first);
        for _ in 1..8 {
            seen.push(pick(&mut state, RowPart::Next));
        }
        let mut all = seen.clone();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 8, "every one of them once: {seen:?}");
        assert!(seen.iter().all(|&i| state.report().issues[i].kind == "mid_air"));
        assert_eq!(pick(&mut state, RowPart::Next), first, "past the eighth it wraps to the first");
        assert_eq!(pick(&mut state, RowPart::Prev), seen[7], "back from the first is the eighth");
        assert_eq!(pick(&mut state, RowPart::Row), seen[7], "the row shows the one it shows");

        // the other kind's row starts at its own first, and Back from none goes to the last
        let thin = &kinds[1].of;
        assert_eq!(issue_pressed(thin, state.picked_issue, RowPart::Row), Some(3));
        assert!(apply(&mut state, &PrintAction::SelectIssue(3)));
        assert_eq!(state.play.layer_hi, 2, "the layer at its height (0.5 mm in layers 0.2 mm high)");
        assert_eq!(issue_pressed(mid_air, Some(3), RowPart::Prev), Some(*mid_air.last().unwrap()));
    }
}

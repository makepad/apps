//! ONE LANE OF THE PALETTE ANIMATOR (donor `palette_animator_view`,
//! deleted in ef48f3d9): a strip as wide as the loop, drawn as the colour
//! the lane gives across it, with the grid, the keyframes and the playhead
//! over that.
//!
//! The lane owns no keyframe: the parent hands it the colours across the
//! loop, the keyframes, the grid and the playhead, and it hands back what
//! the hand did: a press on an empty place (`Add` at a beat), a press on a
//! keyframe (`Choose`), a drag of the chosen keyframe (`Drag` to a beat,
//! once the hand has moved a few points) and the release. The parent edits
//! its animator, which snaps a beat and keeps a keyframe between its
//! neighbours, and hands the result back.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    // One column of the lane's colour, from its left end to its right.
    set_type_default() do #(DrawLaneBand::script_shader(vm)){
        ..mod.draw.DrawQuad
        pixel: fn() {
            let c = self.color0.mix(self.color1, clamp(self.pos.x, 0.0, 1.0))
            return vec4(c.xyz, 1.0)
        }
    }

    // A keyframe: a dot of its colour, ringed; the chosen one in the accent.
    set_type_default() do #(DrawLaneKey::script_shader(vm)){
        ..mod.draw.DrawQuad
        ring: uniform(stage.text_hi)
        chosen_ring: uniform(stage.accent)
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            let r = self.rect_size.x * 0.5
            sdf.circle(r, r, r - 2.0)
            sdf.fill_keep(vec4(self.key_color.xyz, 1.0))
            sdf.stroke(self.ring.mix(self.chosen_ring, self.chosen), 1.5 + self.chosen)
            return sdf.result
        }
    }

    mod.widgets.StagePaletteLaneBase = #(PaletteLane::register_widget(vm))
    mod.widgets.StagePaletteLane = set_type_default() do mod.widgets.StagePaletteLaneBase{
        width: Fill
        height: 24
        draw_bg +: {color: #x00000000}
        draw_line +: {color: #xffffff30}
        draw_bar +: {color: #xffffff70}
        draw_head +: {color: stage.text_hi}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawLaneBand {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub color0: Vec4f,
    #[live]
    pub color1: Vec4f,
}

#[derive(Script, ScriptHook)]
#[repr(C)]
pub struct DrawLaneKey {
    #[deref]
    pub draw_super: DrawQuad,
    #[live]
    pub key_color: Vec4f,
    #[live]
    pub chosen: f32,
}

/// One keyframe as the lane draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LaneKey {
    pub beat: f64,
    pub color: Vec4f,
}

/// What the lane draws.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct LaneView {
    /// The lane's colour at evenly spaced places across the loop, the first
    /// at its start and the last at its end.
    pub colors: Vec<Vec4f>,
    pub keys: Vec<LaneKey>,
    pub loop_beats: f64,
    /// Beats between two grid lines; a line on every bar (4 beats) is
    /// drawn stronger.
    pub grid_beats: f64,
    pub chosen: Option<usize>,
    /// Where the loop stands now, in beats.
    pub playhead: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PaletteLaneAction {
    #[default]
    None,
    /// A press on an empty place, at this beat (not snapped).
    Add(f64),
    /// A press on keyframe `index`.
    Choose(usize),
    /// Keyframe `index` dragged to this beat (not snapped).
    Drag(usize, f64),
    /// The hand let go of a drag.
    Release,
}

/// Points a press may move before it is a drag.
const DRAG_POINTS: f64 = 3.0;
/// The keyframe dot's size.
const KEY_SIZE: f64 = 14.0;

#[derive(Script, ScriptHook, Widget)]
pub struct PaletteLane {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_band: DrawLaneBand,
    #[live]
    draw_line: DrawColor,
    #[live]
    draw_bar: DrawColor,
    #[live]
    draw_head: DrawColor,
    #[live]
    draw_key: DrawLaneKey,
    #[rust]
    view: LaneView,
    /// The keyframe a press took, where it went down, and whether it moved.
    #[rust]
    held: Option<(usize, f64, bool)>,
}

impl PaletteLane {
    pub fn set_view(&mut self, cx: &mut Cx, view: LaneView) {
        if self.view != view {
            self.view = view;
            self.draw_bg.redraw(cx);
        }
    }

    fn beat_at(&self, rect: Rect, x: f64) -> f64 {
        if rect.size.x <= 0.0 {
            return 0.0;
        }
        (x - rect.pos.x) / rect.size.x * self.view.loop_beats
    }

    fn key_at(&self, rect: Rect, x: f64) -> Option<usize> {
        if self.view.loop_beats <= 0.0 {
            return None;
        }
        self.view
            .keys
            .iter()
            .enumerate()
            .map(|(index, key)| (index, (rect.pos.x + key.beat / self.view.loop_beats * rect.size.x - x).abs()))
            .filter(|(_, distance)| *distance <= KEY_SIZE * 0.5)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(index, _)| index)
    }
}

impl Widget for PaletteLane {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let rect = self.draw_bg.area().rect(cx);
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerDown(fe) if fe.is_primary_hit() => match self.key_at(rect, fe.abs.x) {
                Some(index) => {
                    self.held = Some((index, fe.abs.x, false));
                    cx.widget_action(self.uid, PaletteLaneAction::Choose(index));
                }
                None => {
                    self.held = None;
                    cx.widget_action(self.uid, PaletteLaneAction::Add(self.beat_at(rect, fe.abs.x)));
                }
            },
            Hit::FingerMove(fe) => {
                if let Some((index, from, moved)) = self.held {
                    if moved || (fe.abs.x - from).abs() >= DRAG_POINTS {
                        self.held = Some((index, from, true));
                        cx.widget_action(self.uid, PaletteLaneAction::Drag(index, self.beat_at(rect, fe.abs.x)));
                    }
                }
            }
            Hit::FingerUp(_) => {
                if let Some((_, _, moved)) = self.held.take() {
                    if moved {
                        cx.widget_action(self.uid, PaletteLaneAction::Release);
                    }
                }
            }
            Hit::FingerHoverIn(_) | Hit::FingerHoverOver(_) => cx.set_cursor(MouseCursor::Hand),
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let rect = cx.walk_turtle(walk);
        self.draw_bg.draw_abs(cx, rect);
        let view = &self.view;
        let band = Rect { pos: dvec2(rect.pos.x, rect.pos.y + 3.0), size: dvec2(rect.size.x, (rect.size.y - 6.0).max(1.0)) };
        let columns = view.colors.len().saturating_sub(1);
        for column in 0..columns {
            let x0 = band.pos.x + band.size.x * column as f64 / columns as f64;
            let x1 = band.pos.x + band.size.x * (column + 1) as f64 / columns as f64;
            self.draw_band.color0 = view.colors[column];
            self.draw_band.color1 = view.colors[column + 1];
            // A hair of overlap so two columns meet with no seam.
            self.draw_band.draw_abs(cx, Rect { pos: dvec2(x0, band.pos.y), size: dvec2(x1 - x0 + 0.5, band.size.y) });
        }
        if view.loop_beats > 0.0 && view.grid_beats > 0.0 {
            let lines = (view.loop_beats / view.grid_beats).round() as usize;
            for line in 1..lines {
                let beat = line as f64 * view.grid_beats;
                let x = band.pos.x + beat / view.loop_beats * band.size.x;
                let bar = (beat / 4.0).fract().abs() < 1e-9;
                let at = Rect { pos: dvec2(x, band.pos.y), size: dvec2(1.0, band.size.y) };
                if bar { self.draw_bar.draw_abs(cx, at) } else { self.draw_line.draw_abs(cx, at) }
            }
            if let Some(head) = view.playhead {
                let x = band.pos.x + head.rem_euclid(view.loop_beats) / view.loop_beats * band.size.x;
                self.draw_head.draw_abs(cx, Rect { pos: dvec2(x - 1.0, rect.pos.y), size: dvec2(2.0, rect.size.y) });
            }
            for (index, key) in view.keys.iter().enumerate() {
                let x = band.pos.x + key.beat / view.loop_beats * band.size.x;
                self.draw_key.key_color = key.color;
                self.draw_key.chosen = if view.chosen == Some(index) { 1.0 } else { 0.0 };
                let y = rect.pos.y + (rect.size.y - KEY_SIZE) * 0.5;
                self.draw_key.draw_abs(cx, Rect { pos: dvec2(x - KEY_SIZE * 0.5, y), size: dvec2(KEY_SIZE, KEY_SIZE) });
            }
        }
        DrawStep::done()
    }
}

impl PaletteLaneRef {
    pub fn set_view(&self, cx: &mut Cx, view: LaneView) {
        if let Some(mut inner) = self.borrow_mut() {
            inner.set_view(cx, view);
        }
    }

    /// What the hand did on this lane in `actions`.
    pub fn action(&self, actions: &Actions) -> Option<PaletteLaneAction> {
        let action: PaletteLaneAction = actions.find_widget_action(self.widget_uid())?.cast();
        (action != PaletteLaneAction::None).then_some(action)
    }
}

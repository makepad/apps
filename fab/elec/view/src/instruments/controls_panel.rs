//! The bench's controls, generated from the design (user, 11-18: the approved mock, "all switches
//! and potentiometers visible"): one control per switch and pot of the running board
//! ([`panel_model`]), in the groups and arrangement the design gives them. A key is a cap with its
//! label centred and a dot in its corner lit by the LED beside it on the board; a knob turns with
//! a vertical drag and shows its label and value under it. Groups sit side by side and wrap when
//! the pane narrows; no control is ever left out.
//!
//! Per frame: the model is made once per lowering (O(parts)); a draw is O(controls) quads and
//! labels; a press or a turn sends one command.

use super::look::InstrumentLook;
use super::panel_model::{arranged, panel_model, ControlKind, Group, PanelModel};
use crate::live::sim::SimAction;
use crate::snapshot::ElecLive;
use crate::trace::cursor::eng;
use fab_elec_bench::SimCmd;
use fab_elec_core::{BoardId, CompId, CompRef};
use makepad_widgets_core::*;
use std::sync::Arc;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    // A rounded box: a group's panel, a key's cap, a knob's body.
    set_type_default() do #(DrawPanelBox::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        frame: #0000
        radius: 2.5
        pixel: fn() {
            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, self.radius)
            sdf.fill_keep(self.color)
            sdf.stroke(self.frame, 1.0)
            return sdf.result
        }
    }

    mod.widgets.ControlsPanelBase = #(ControlsPanel::register_widget(vm))
    mod.widgets.ControlsPanel = set_type_default() do mod.widgets.ControlsPanelBase {
        width: Fill height: Fit
        draw_text +: {text_style: theme.font_regular {font_size: theme.prop_font_size}}
        draw_title +: {text_style: theme.font_regular {font_size: 7.5}}
        draw_code +: {text_style: theme.font_code {font_size: theme.prop_font_size}}
        // the sheet's corners; Fab sets its tokens on its instances (fab.radius / fab.radius_lg)
        radius: theme.corner_radius
        radius_lg: theme.container_corner_radius
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawPanelBox {
    #[deref]
    draw_super: DrawQuad,
    #[live]
    color: Vec4f,
    #[live]
    frame: Vec4f,
    #[live]
    radius: f32,
}

const KEY: DVec2 = DVec2 { x: 36.0, y: 22.0 };
/// The narrowest a key gets in a narrow pane.
const KEY_MIN: f64 = 22.0;
/// A lamp's cell: an LED no control carries, its dot over its reference.
const LAMP: DVec2 = DVec2 { x: 36.0, y: 24.0 };
const KNOB: f64 = 22.0;
const KNOB_CELL: DVec2 = DVec2 { x: 54.0, y: 50.0 };
const PITCH: f64 = 3.0;
const PAD: f64 = 6.0;
const TITLE_H: f64 = 15.0;

/// One control as drawn, for a press and for tools: its part, its kind, where it is, and its
/// label and value as shown.
#[derive(Clone)]
struct Placed {
    comp: CompRef,
    kind: ControlKind,
    rect: Rect,
    label: String,
    value: String,
    /// A knob's place, 0 (counter-clockwise) to 1 (clockwise); 0 for a key.
    place: f64,
    /// Its rect as an area, which moves with the panel as its host lays it out: its bounds for
    /// tools once the frame's layout is done.
    area: Area,
}

/// A knob's turn per assistive increment or decrement: a hundredth of its travel.
const KNOB_STEP: f64 = 0.01;

#[derive(Script, ScriptHook, Widget)]
pub struct ControlsPanel {
    #[uid]
    uid: WidgetUid,
    #[source]
    script_source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[live]
    draw_box: DrawPanelBox,
    /// The corners, in Fab's tokens (drawn at twice the value): a key's, and a group's.
    #[live]
    radius: f64,
    #[live]
    radius_lg: f64,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_title: DrawText,
    #[live]
    draw_code: DrawText,
    #[redraw]
    #[rust]
    area: Area,
    /// The model and the lowering generation it was made for.
    #[rust]
    model: Option<((u64, usize, usize), Arc<PanelModel>)>,
    #[rust]
    placed: Vec<Placed>,
    /// The scroll down the controls, its most, and the room they were drawn in.
    #[rust]
    scroll_y: f64,
    #[rust]
    scroll_max: f64,
    #[rust]
    view: Rect,
    /// Where the panel was drawn, for its controls' bounds as it stands now.
    #[rust]
    drawn_at: DVec2,
    /// A knob being turned: its comp, the pointer's y and the place where the turn began.
    #[rust]
    turning: Option<(CompRef, f64, f64)>,
    /// A key held down by the pointer.
    #[rust]
    pressing: Option<CompRef>,
    /// What the panel showed last: it draws again when this moves.
    #[rust]
    shown: Option<(usize, usize, usize, u64)>,
    #[rust]
    text: String,
}

fn rgba(c: [f32; 4], a: f32) -> Vec4f {
    vec4(c[0], c[1], c[2], c[3] * a)
}

/// Whether a model has nothing to show: no control and no lamp.
fn out_empty(model: &PanelModel) -> bool {
    model.groups.is_empty() && model.indicators.is_empty()
}

fn mix(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t)
}

impl ControlsPanel {
    fn key(cx: &Cx) -> (usize, usize, usize, u64) {
        let Some(live) = cx.get_global_ref::<ElecLive>() else { return (0, 0, 0, 0) };
        (Arc::as_ptr(&live.sim.glow) as usize, Arc::as_ptr(&live.sim.held) as usize, Arc::as_ptr(&live.sim.turned) as usize, live.sim.index.generation)
    }

    /// The model of the run bench's board, made again for each lowering.
    fn sync_model(&mut self, cx: &Cx) -> Option<(BoardId, Arc<PanelModel>)> {
        let live = cx.get_global_ref::<ElecLive>()?;
        let bench = live.run_bench()?;
        let board = bench.out.scope_id();
        let key = (live.sim.index.generation, Arc::as_ptr(&live.snap) as usize, bench as *const _ as usize);
        if let Some((seen, model)) = &self.model {
            if *seen == key {
                return Some((board, model.clone()));
            }
        }
        let unit = live.snap.live().unit(board)?;
        // the bench's `panel` option arranges the generated controls; it never hides one
        let model = panel_model(unit);
        let model = Arc::new(match &bench.out.panel {
            Some(panel) => arranged(model, panel),
            None => model,
        });
        self.model = Some((key, model.clone()));
        Some((board, model))
    }

    fn text_width(&mut self, cx: &mut Cx2d, which: u8, text: &str) -> DVec2 {
        let draw = match which { 0 => &mut self.draw_text, 1 => &mut self.draw_title, _ => &mut self.draw_code };
        let laid = draw.layout(cx, 0.0, 0.0, None, false, Align::default(), text);
        dvec2(f64::from(laid.size_in_lpxs.width * draw.font_scale), f64::from(laid.size_in_lpxs.height * draw.font_scale))
    }

    fn centred(&mut self, cx: &mut Cx2d, which: u8, rect: Rect, text: &str, color: Vec4f) {
        let size = self.text_width(cx, which, text);
        let at = rect.pos + dvec2(((rect.size.x - size.x) * 0.5).max(0.0), ((rect.size.y - size.y) * 0.5).max(0.0));
        let draw = match which { 0 => &mut self.draw_text, 1 => &mut self.draw_title, _ => &mut self.draw_code };
        draw.color = color;
        draw.draw_abs(cx, at, text);
    }

    fn rounded(&mut self, cx: &mut Cx2d, rect: Rect, fill: Vec4f, frame: Vec4f, radius: f32) {
        self.draw_box.color = fill;
        self.draw_box.frame = frame;
        self.draw_box.radius = radius;
        self.draw_box.draw_abs(cx, rect);
    }

    /// A group's size with keys `key_w` wide: its grid of keys, or its row of knob cells, and its
    /// title.
    fn group_size(g: &Group, key_w: f64) -> DVec2 {
        let knobs = g.controls.iter().any(|c| matches!(c.kind, ControlKind::Knob | ControlKind::Slider));
        let cell = if knobs { KNOB_CELL } else { dvec2(key_w + PITCH, KEY.y + PITCH) };
        let (rows, cols) = (f64::from(g.rows.max(1)), f64::from(g.cols.max(1)));
        dvec2(cols * cell.x - if knobs { 0.0 } else { PITCH } + 2.0 * PAD, TITLE_H + rows * cell.y - if knobs { 0.0 } else { PITCH } + PAD)
    }

    /// The groups in a panel `width` wide, then the lamps (the LEDs no control carries) when there
    /// are any: each one's place, size and key width (the lamps': their columns), and the panel's
    /// height. Groups sit side by side and wrap to another row; a key group wider than the panel
    /// narrows its keys to fit (to [`KEY_MIN`]). O(groups).
    fn layout(model: &PanelModel, width: f64) -> (Vec<(DVec2, DVec2, f64)>, f64) {
        let room = (width - 4.0 * PAD).max(KEY_MIN);
        let (mut x, mut y, mut row_h) = (PAD, PAD, 0.0f64);
        let mut out = Vec::with_capacity(model.groups.len());
        // the groups, then the LEDs no control carries as one group of lamps
        let lamps = model.indicators.len();
        let lamp_cols = ((room / LAMP.x).floor().max(1.0) as usize).min(lamps.max(1));
        let lamp_size = dvec2(lamp_cols as f64 * LAMP.x + 2.0 * PAD, TITLE_H + lamps.div_ceil(lamp_cols) as f64 * LAMP.y + PAD * 0.5);
        let sizes = model.groups.iter().map(|g| {
            let cols = f64::from(g.cols.max(1));
            let key_w = ((room - (cols - 1.0) * PITCH) / cols).clamp(KEY_MIN, KEY.x);
            (Self::group_size(g, key_w), key_w)
        }).chain((lamps > 0).then_some((lamp_size, lamp_cols as f64)));
        for (size, key_w) in sizes {
            if x > PAD && x + size.x > width - PAD {
                x = PAD;
                y += row_h + PAD;
                row_h = 0.0;
            }
            out.push((dvec2(x, y), size, key_w));
            x += size.x + PAD;
            row_h = row_h.max(size.y);
        }
        (out, if out_empty(model) { 0.0 } else { y + row_h + PAD })
    }

    /// Counts the board's keys and switches, then its knobs and sliders.
    /// Whether the run bench's `panel` option arranges the controls (else they are as generated).
    pub fn by_bench(&self, cx: &Cx) -> bool {
        cx.get_global_ref::<ElecLive>().and_then(ElecLive::run_bench).is_some_and(|bench| bench.out.panel.is_some())
    }

    pub fn counts(&mut self, cx: &Cx) -> Option<(usize, usize)> {
        let (_, model) = self.sync_model(cx)?;
        let (mut keys, mut knobs) = (0, 0);
        for control in model.groups.iter().flat_map(|group| &group.controls) {
            match control.kind {
                ControlKind::Key | ControlKind::Switch => keys += 1,
                ControlKind::Knob | ControlKind::Slider => knobs += 1,
            }
        }
        Some((keys, knobs))
    }

    /// The panel's height at `width`: 0 with no run bench or no control on its board.
    pub fn height_for(&mut self, cx: &Cx, width: f64) -> f64 {
        self.sync_model(cx).map_or(0.0, |(_, model)| Self::layout(&model, width).1)
    }

    /// Draws the panel into `rect`: its [`ControlsPanel::height_for`] of its width, or less, where
    /// the controls scroll (the wheel over them, a thumb at the right edge).
    pub fn draw_in(&mut self, cx: &mut Cx2d, rect: Rect) {
        cx.add_rect_area(&mut self.area, rect);
        self.drawn_at = rect.pos;
        self.placed.clear();
        // what it shows, for every host (some draw it here, not through `draw_walk`)
        self.shown = Some(Self::key(cx));
        let Some((board, model)) = self.sync_model(cx) else { return };
        let (glow, glow_parts, held, turned, starts) = match cx.get_global_ref::<ElecLive>() {
            Some(live) => (live.sim.glow.clone(), live.sim.glow_parts.clone(), live.sim.held.clone(), live.sim.turned.clone(), live.sim.index.knobs().to_vec()),
            None => return,
        };
        let look = InstrumentLook::for_theme(base_theme(cx) != BaseTheme::Light);
        let dark = base_theme(cx) != BaseTheme::Light;
        let black = [0.0, 0.0, 0.0, 1.0];
        let ground = if dark { mix(look.canvas.canvas, black, 0.25) } else { look.canvas.canvas };
        let panel = look.canvas.part_fill;
        let cap = if dark { mix(look.canvas.part_fill, look.text, 0.08) } else { look.canvas.block_fill };
        let (places, content_h) = Self::layout(&model, rect.size.x);
        self.rounded(cx, rect, rgba(ground, 1.0), rgba(ground, 0.0), 0.0);
        // taller than its room: the controls scroll under a clip, every one reachable
        self.scroll_max = (content_h - rect.size.y).max(0.0);
        self.scroll_y = self.scroll_y.clamp(0.0, self.scroll_max);
        self.view = rect;
        let origin = rect.pos - dvec2(0.0, self.scroll_y);
        cx.push_clip_rect(rect);
        let level_of = |comp: CompId| glow_parts.iter().position(|p| p.board == board && p.comp == comp).map(|i| (i, glow.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0)));
        for (group, &(at, size, key_w)) in model.groups.iter().zip(&places) {
            let g = Rect { pos: origin + at, size };
            self.rounded(cx, g, rgba(panel, 1.0), rgba(look.hairline, 1.0), self.radius_lg as f32);
            let title = group.name.to_uppercase();
            self.draw_title.color = rgba(look.text_tertiary, 1.0);
            self.draw_title.draw_abs(cx, g.pos + dvec2(PAD, 3.0), &title);
            let knobs = group.controls.iter().any(|c| matches!(c.kind, ControlKind::Knob | ControlKind::Slider));
            for c in &group.controls {
                let comp = CompRef { board, comp: c.comp };
                let cell = if knobs { KNOB_CELL } else { dvec2(key_w + PITCH, KEY.y + PITCH) };
                let pos = g.pos + dvec2(PAD + f64::from(c.col) * cell.x, TITLE_H + f64::from(c.row) * cell.y);
                match c.kind {
                    ControlKind::Key | ControlKind::Switch => {
                        let r = Rect { pos, size: dvec2(key_w, KEY.y) };
                        let down = held.contains(&comp) || self.pressing == Some(comp);
                        let lit = c.glow.and_then(level_of);
                        let led_rgb = lit.and_then(|(i, _)| self.led_colour(cx, glow_parts[i])).unwrap_or(look.canvas.violation);
                        let level = lit.map_or(0.0, |(_, l)| l);
                        let fill = if down { mix(cap, look.canvas.selection, 0.45) } else { mix(cap, led_rgb, 0.45 * level) };
                        self.rounded(cx, r, rgba(fill, 1.0), rgba(look.zero_line, 1.0), self.radius as f32);
                        if lit.is_some() {
                            let dot = Rect { pos: r.pos + dvec2(r.size.x - 7.0, 3.0), size: dvec2(4.0, 4.0) };
                            self.rounded(cx, dot, rgba(mix(look.text_tertiary, led_rgb, level), 0.4 + 0.6 * level), rgba(led_rgb, 0.0), 2.5);
                        }
                        self.centred(cx, 0, r, &c.label, rgba(look.text, 1.0));
                        let value = if down { "down" } else if level > 0.5 { "lit" } else { "" };
                        let mut area = Area::Empty;
                        cx.add_rect_area(&mut area, r);
                        self.placed.push(Placed { comp, kind: c.kind, rect: r, label: c.label.clone(), value: value.to_owned(), place: 0.0, area });
                    }
                    ControlKind::Knob | ControlKind::Slider => {
                        let start = starts.iter().find(|k| k.0 == comp).map_or(0.5, |k| k.1);
                        let place = turned.iter().find(|t| t.0 == comp).map_or(start, |t| t.1).clamp(0.0, 1.0);
                        let body = Rect { pos: pos + dvec2((KNOB_CELL.x - KNOB) * 0.5, 2.0), size: dvec2(KNOB, KNOB) };
                        self.rounded(cx, body, rgba(cap, 1.0), rgba(look.zero_line, 1.0), (KNOB * 0.5) as f32);
                        // the pointer: -135 deg at 0, +135 deg at 1
                        let a = (-135.0 + 270.0 * place).to_radians();
                        let centre = body.pos + body.size * 0.5;
                        for k in 0..6 {
                            let r = 2.5 + f64::from(k) * 1.4;
                            let p = centre + dvec2(a.sin() * r, -a.cos() * r);
                            self.rounded(cx, Rect { pos: p - dvec2(1.0, 1.0), size: dvec2(2.0, 2.0) }, rgba(look.text, 1.0), rgba(look.text, 0.0), 1.0);
                        }
                        let label_r = Rect { pos: pos + dvec2(0.0, KNOB + 3.0), size: dvec2(KNOB_CELL.x, 12.0) };
                        self.centred(cx, 1, label_r, &c.label, rgba(look.text_secondary, 1.0));
                        self.text.clear();
                        self.text.push_str(&self.knob_value(cx, comp, place));
                        let value = std::mem::take(&mut self.text);
                        let value_r = Rect { pos: label_r.pos + dvec2(0.0, 12.0), size: label_r.size };
                        self.centred(cx, 2, value_r, &value, rgba(look.text, 1.0));
                        let mut area = Area::Empty;
                        cx.add_rect_area(&mut area, body);
                        self.placed.push(Placed { comp, kind: c.kind, rect: body, label: c.label.clone(), value: value.clone(), place, area });
                        self.text = value;
                    }
                }
            }
        }
        // the LEDs no control carries: each one's dot lit as it glows, its reference under it
        if let Some(&(at, size, cols)) = places.get(model.groups.len()) {
            let g = Rect { pos: origin + at, size };
            self.rounded(cx, g, rgba(panel, 1.0), rgba(look.hairline, 1.0), self.radius_lg as f32);
            self.draw_title.color = rgba(look.text_tertiary, 1.0);
            self.draw_title.draw_abs(cx, g.pos + dvec2(PAD, 3.0), "LEDS");
            let cols = (cols as usize).max(1);
            for (i, &led) in model.indicators.iter().enumerate() {
                let cell = Rect { pos: g.pos + dvec2(PAD + (i % cols) as f64 * LAMP.x, TITLE_H + (i / cols) as f64 * LAMP.y), size: LAMP };
                let led_ref = CompRef { board, comp: led };
                let level = level_of(led).map_or(0.0, |(_, l)| l);
                let rgb = self.led_colour(cx, led_ref).unwrap_or(look.canvas.violation);
                let dot = Rect { pos: cell.pos + dvec2((LAMP.x - 8.0) * 0.5, 2.0), size: dvec2(8.0, 8.0) };
                self.rounded(cx, dot, rgba(mix(cap, rgb, level), 1.0), rgba(look.zero_line, 1.0), 4.0);
                let name = self.refdes(cx, led_ref);
                self.centred(cx, 1, Rect { pos: cell.pos + dvec2(0.0, 11.0), size: dvec2(LAMP.x, 12.0) }, &name, rgba(look.text_secondary, 1.0));
            }
        }
        cx.pop_clip_rect();
        // the scroll's thumb at the right edge while the controls are taller than the room
        if self.scroll_max > 0.0 {
            let track = rect.size.y - 8.0;
            let thumb = (track * rect.size.y / content_h).max(24.0).min(track);
            let y = rect.pos.y + 4.0 + (track - thumb) * self.scroll_y / self.scroll_max;
            self.rounded(cx, Rect { pos: dvec2(rect.pos.x + rect.size.x - 7.0, y), size: dvec2(4.0, thumb) }, rgba(look.text_tertiary, 0.6), rgba(look.text_tertiary, 0.0), 2.0);
        }
    }

    /// A part's reference (`D3`), from the design.
    fn refdes(&self, cx: &Cx, comp: CompRef) -> String {
        cx.get_global_ref::<ElecLive>()
            .and_then(|live| live.snap.live().unit(comp.board).and_then(|unit| unit.circuit.comps.iter().find(|c| c.id == comp.comp)).map(|c| c.refdes.clone()))
            .unwrap_or_default()
    }

    /// The LED's colour as its part emits it, from the design.
    fn led_colour(&self, cx: &Cx, led: CompRef) -> Option<[f32; 4]> {
        let live = cx.get_global_ref::<ElecLive>()?;
        let unit = live.snap.live().unit(led.board)?;
        let comp = unit.circuit.comps.iter().find(|c| c.id == led.comp)?;
        let rgb = crate::canvas::emitter::emitter_rgb(&comp.part.emitter.as_ref()?.light);
        Some([rgb[0], rgb[1], rgb[2], 1.0])
    }

    /// How far the panel has moved since it was drawn, as its host laid it out.
    fn shift(&self, cx: &Cx) -> DVec2 {
        match self.area.rect(cx) {
            r if r.size.x > 0.0 || r.size.y > 0.0 => r.pos - self.drawn_at,
            _ => DVec2::default(),
        }
    }

    /// A control's stable part key in the semantic tree.
    fn part_key(comp: CompRef) -> u64 {
        LiveId::from_str(&format!("control:{}", comp.comp.ix())).0
    }

    /// A knob's value at `place`: the pot's resistance on its wiper's side, or its place in
    /// percent when the design gives no resistance.
    fn knob_value(&self, cx: &Cx, comp: CompRef, place: f64) -> String {
        let ohms = cx.get_global_ref::<ElecLive>().and_then(|live| {
            let unit = live.snap.live().unit(comp.board)?;
            let c = unit.circuit.comps.iter().find(|c| c.id == comp.comp)?;
            c.value.as_ref()?.qty.map(|q| q.si)
        });
        match ohms {
            Some(r) => eng(r * place, "Ω", 3),
            None => format!("{:.0} %", place * 100.0),
        }
    }
}

impl Widget for ControlsPanel {
    /// The panel at `walk`'s height when fixed (a host capping it scrolls the controls), else at
    /// the height its controls need.
    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let width = cx.peek_walk_turtle(walk).size.x;
        let height = match walk.height {
            Size::Fixed(height) => height,
            _ => self.height_for(cx, width),
        };
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(height), ..walk });
        self.draw_in(cx, rect);
        DrawStep::done()
    }

    /// Each control for tools and assistive technology: a key as a button (pressed down while
    /// held) that a press taps, a knob as a slider over its travel (0 counter-clockwise to 1
    /// clockwise) that takes a value or a step either way.
    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        let shift = self.shift(cx);
        for p in &self.placed {
            let key = matches!(p.kind, ControlKind::Key | ControlKind::Switch);
            let node = a11y_tree::Node {
                role: if key { a11y_tree::Role::Button } else { a11y_tree::Role::Slider },
                bounds: makepad_platform::a11y::semantic_rect(Rect { pos: p.rect.pos + shift, ..p.rect }),
                name: p.label.clone(),
                value: p.value.clone(),
                actions: if key {
                    a11y_tree::Actions::PRESS
                } else {
                    a11y_tree::Actions::SET_VALUE | a11y_tree::Actions::INCREMENT | a11y_tree::Actions::DECREMENT
                },
                range: (!key).then_some(a11y_tree::RangeValue { min: 0.0, max: 1.0, now: p.place, step: KNOB_STEP }),
                ..Default::default()
            };
            out.push((Self::part_key(p.comp), None, node));
        }
    }

    /// Each control's own area: its bounds once the panel has moved where its host lays it.
    fn semantics_part_area(&self, key: u64) -> Area {
        self.placed.iter().find(|p| Self::part_key(p.comp) == key).map_or(Area::Empty, |p| p.area)
    }

    /// A press taps a key: down and up at once, and the host holds it for its shortest press of
    /// simulated time, so a firmware's poll sees it however fast the run goes. A knob turns to a
    /// value or by a step, as a drag turns it.
    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionArg, ActionResult, Actions as A};
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(p) = self.placed.iter().find(|p| Self::part_key(p.comp) == key) else { return ActionResult::Unhandled };
        let (comp, place) = (p.comp, p.place);
        match p.kind {
            ControlKind::Key | ControlKind::Switch => {
                if request.action != A::PRESS {
                    return ActionResult::Unsupported;
                }
                cx.action(SimAction::Cmd(SimCmd::press(comp, true)));
                cx.action(SimAction::Cmd(SimCmd::press(comp, false)));
            }
            ControlKind::Knob | ControlKind::Slider => {
                let value = if request.action == A::SET_VALUE {
                    match &request.arg {
                        ActionArg::Number(v) if v.is_finite() => *v,
                        _ => return ActionResult::Unsupported,
                    }
                } else if request.action == A::INCREMENT {
                    place + KNOB_STEP
                } else if request.action == A::DECREMENT {
                    place - KNOB_STEP
                } else {
                    return ActionResult::Unsupported;
                };
                cx.action(SimAction::Cmd(SimCmd::SetParam { comp, value: value.clamp(0.0, 1.0) }));
            }
        }
        self.area.redraw(cx);
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        if matches!(event, Event::NextFrame(_) | Event::Signal) && self.shown != Some(Self::key(cx)) {
            self.area.redraw(cx);
        }
        // the controls' rects are where they were drawn; the panel may have moved since with its host
        let shift = self.shift(cx);
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                let at = e.abs - shift;
                let over = self.placed.iter().any(|p| p.rect.contains(at));
                cx.set_cursor(if over { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerScroll(e) if self.scroll_max > 0.0 => {
                self.scroll_y = (self.scroll_y + e.scroll.y).clamp(0.0, self.scroll_max);
                self.area.redraw(cx);
            }
            Hit::FingerDown(e) => {
                let (view, at) = (self.view, e.abs - shift);
                let Some(p) = self.placed.iter().find(|p| p.rect.contains(at) && view.contains(at)).cloned() else { return };
                match p.kind {
                    ControlKind::Key | ControlKind::Switch => {
                        self.pressing = Some(p.comp);
                        cx.action(SimAction::Cmd(SimCmd::press(p.comp, true)));
                    }
                    ControlKind::Knob | ControlKind::Slider => {
                        let place = cx.get_global_ref::<ElecLive>().and_then(|live| {
                            live.sim.turned.iter().find(|t| t.0 == p.comp).map(|t| t.1)
                                .or_else(|| live.sim.index.knobs().iter().find(|k| k.0 == p.comp).map(|k| k.1))
                        }).unwrap_or(0.5);
                        self.turning = Some((p.comp, e.abs.y, place));
                    }
                }
                self.area.redraw(cx);
            }
            Hit::FingerMove(e) => {
                if let Some((comp, y0, place0)) = self.turning {
                    let value = (place0 + (y0 - e.abs.y) / 150.0).clamp(0.0, 1.0);
                    cx.action(SimAction::Cmd(SimCmd::SetParam { comp, value }));
                    self.area.redraw(cx);
                }
            }
            Hit::FingerUp(_) => {
                if let Some(comp) = self.pressing.take() {
                    cx.action(SimAction::Cmd(SimCmd::press(comp, false)));
                }
                self.turning = None;
                self.area.redraw(cx);
            }
            _ => {}
        }
    }
}

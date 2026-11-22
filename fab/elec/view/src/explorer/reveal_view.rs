//! A retained reveal above the sheet. Only clocks and uniforms change during flight.

use super::{
    liveix::SceneLive,
    scene::{Flight, FlyFrom, Leg, LookupPart, Scene, SceneState, TEXT_MIN_POINTS, text_points},
    view::{draw_mini_layer, mini_alpha, Templates},
};
use crate::canvas::{
    chips::{Chip, ChipAnchor, ChipKind, ChipLayer},
    draw::{BoxInst, CanvasLayer, CanvasLive, CanvasUniforms, Common, DrawCanvasArc, DrawCanvasBox, DrawCanvasBridge, DrawCanvasLine, DrawCanvasMini, DrawCanvasPin},
    looks::{MOVE_SECONDS, REVEAL_SECONDS, STATE_SECONDS},
    pane::{node_part, part_key, HIT_RADIUS},
    Camera2d, CanvasItems, CanvasNode, Tokens,
};
use crate::board::nodes::natural_cmp;
use fab_elec_core::Sel;
use fab_elec_design::Design;
use fab_elec_explore::{reveal::RevealSheet, rows::RowIndex, SheetLayout};
use makepad_widgets_core::makepad_platform::a11y_tree;
use makepad_widgets_core::*;
use std::sync::Arc;

struct TrailEntry {
    id: u32,
    focus: u32,
    text: String,
}

pub struct Reveal {
    pub data: RevealSheet,
    pub scene: Scene,
    display: SheetLayout,
    design: Arc<Design>,
    rows: Arc<RowIndex>,
    /// The shared net, part and pin rows and this reveal's own wire segment rows.
    live: Arc<SceneLive>,
    unit: usize,
    state: SceneState,
    boxes: CanvasLayer,
    lines: CanvasLayer,
    /// Retained strips and bundle member rows for this reveal's own subscriptions.
    minis: CanvasLayer,
    /// Firmware pin marks for this reveal's part poses.
    pins: CanvasLayer,
    arcs: CanvasLayer,
    bridges: CanvasLayer,
    bridges_end: Option<f64>,
    leaders: CanvasLayer,
    backdrop: CanvasLayer,
    backdrop_fade: Flight,
    ghost: Flight,
    home_end: Option<f64>,
    trim_at: Option<f64>,
    trail: Vec<TrailEntry>,
    trail_id: u32,
    chips: ChipLayer,
    trail_origin: Option<[f64; 2]>,
}

fn fade(from: f32, to: f32, start: f64, duration: f64) -> Flight {
    Flight { from: [0.0, 0.0, 0.0, from], to: [0.0, 0.0, 0.0, to], span: [start as f32, duration as f32] }
}

impl Reveal {
    pub(super) fn new(
        data: RevealSheet, previous: Option<Self>, sheet: &SheetLayout, sheet_scene: &Scene,
        design: &Arc<Design>, unit: usize, rows: &Arc<RowIndex>, live: Arc<SceneLive>, tokens: &Tokens, templates: &Templates, now: f64,
    ) -> Self {
        let mut display = data.sheet.clone();
        // Relations leaving this reveal finish their trip home instead of disappearing.
        let mut retiring = false;
        if let Some(old) = &previous {
            for (n, p) in display.parts.iter_mut().enumerate() {
                if !p.placed && old.display.parts.get(n).is_some_and(|p| p.placed) {
                    if let Some(home) = sheet.parts.get(n).filter(|p| p.placed) {
                        *p = *home;
                        retiring = true;
                    }
                }
            }
        }
        let mut parts = vec![None; display.parts.len()];
        let mut landed = now + MOVE_SECONDS;
        for (k, (n, p)) in display.parts.iter().enumerate().filter(|(_, p)| p.placed).enumerate() {
            let current = previous.as_ref().and_then(|old| old.scene.position(n as u32, now))
                .or_else(|| sheet_scene.position(n as u32, now))
                .unwrap_or_else(|| sheet.parts[n].at.map(f64::from));
            let motion = Flight {
                from: [(current[0] - p.at[0] as f64) as f32, (current[1] - p.at[1] as f64) as f32, 0.0, 1.0],
                to: [0.0, 0.0, 0.0, 1.0],
                span: [(now + 0.01 * (k % 24) as f64) as f32, REVEAL_SECONDS as f32],
            };
            landed = landed.max(motion.end());
            parts[n] = Some(motion);
        }
        let ghost = previous.as_ref().map_or(1.0, |old| old.sheet_alpha(now));
        let continued = previous.is_some();
        let mut text_from = Vec::new();
        if let Some(old) = &previous {
            text_from.resize(display.parts.len(), 0.0f32);
            for text in &old.scene.texts {
                if let Some(alpha) = text.owner.and_then(|node| text_from.get_mut(node as usize)) {
                    *alpha = alpha.max(text.alpha(now));
                }
            }
        }
        let mut trail = Vec::new();
        let mut trail_id = 0;
        let mut chips = ChipLayer::default();
        if let Some(mut old) = previous {
            if old.home_end.is_none() {
                trail = std::mem::take(&mut old.trail);
                trail_id = old.trail_id;
                chips = old.chips;
                if old.data.focus != data.focus {
                    if let Some(comp) = design.units[unit].circuit.comps.get(old.data.focus as usize) {
                        trail_id += 1;
                        trail.push(TrailEntry { id: trail_id, focus: old.data.focus, text: comp.refdes.clone() });
                        if trail.len() > 6 {
                            trail.remove(0);
                            // ChipLayer retains fading-out entries; cap the visible trail too.
                            chips = ChipLayer::default();
                        }
                    }
                }
            }
        }
        let mut out = Self {
            state: SceneState {
                fly: Some(FlyFrom { parts, detail: fade(0.0, 1.0, landed, STATE_SECONDS),
                    leg: Leg::Arrive, start: now, continued, text_from }),
                ..SceneState::default()
            },
            data, display, design: design.clone(), rows: rows.clone(), live, unit, scene: Scene::default(),
            boxes: CanvasLayer::default(), lines: CanvasLayer::default(), arcs: CanvasLayer::default(),
            bridges: CanvasLayer::default(), bridges_end: None,
            minis: CanvasLayer::default(),
            pins: CanvasLayer::default(),
            leaders: CanvasLayer::default(), backdrop: CanvasLayer::default(),
            backdrop_fade: fade(0.0, 1.0, now, STATE_SECONDS),
            ghost: fade(ghost, 0.3, now, STATE_SECONDS),
            home_end: None, trim_at: retiring.then_some(landed),
            trail, trail_id, chips, trail_origin: None,
        };
        out.rebuild(tokens, templates);
        out
    }

    /// Refresh the lowering's net and part rows and the reveal wires' own segment rows.
    pub(super) fn set_live(&mut self, live: Arc<SceneLive>, tokens: &Tokens, templates: &Templates) {
        self.live = live;
        self.rebuild(tokens, templates);
    }

    pub(super) fn rebuild(&mut self, tokens: &Tokens, templates: &Templates) {
        self.scene = Scene::build(&self.design, self.unit, &self.display, &self.rows, tokens, &self.state, &self.live);
        let mut boxes = Vec::with_capacity(self.scene.boxes.len() * templates.boxes.slots());
        let mut lines = Vec::with_capacity(self.scene.lines.len() * templates.line.slots());
        let mut arcs = Vec::with_capacity(self.scene.arcs.len() * templates.arc.slots());
        let mut bridges = Vec::with_capacity(self.scene.bridges.len() * templates.bridge.slots());
        let mut leaders = Vec::with_capacity(self.scene.leaders.len() * templates.line.slots());
        let mut minis = Vec::with_capacity(self.scene.minis.len() * templates.mini.slots());
        let mut pins = Vec::with_capacity(self.scene.pins.len() * templates.pin.slots());
        for row in &self.scene.boxes { templates.boxes.append(&mut boxes, row); }
        for row in &self.scene.lines { templates.line.append(&mut lines, row); }
        for row in &self.scene.arcs { templates.arc.append(&mut arcs, row); }
        for row in &self.scene.bridges { templates.bridge.append(&mut bridges, row); }
        for row in &self.scene.leaders { templates.line.append(&mut leaders, row); }
        for row in &self.scene.minis { templates.mini.append(&mut minis, row); }
        for row in &self.scene.pins { templates.pin.append(&mut pins, row); }
        self.boxes.set(templates.boxes.slots(), boxes);
        self.lines.set(templates.line.slots(), lines);
        self.arcs.set(templates.arc.slots(), arcs);
        self.bridges.set(templates.bridge.slots(), bridges);
        self.bridges_end = self.scene.bridges.iter().map(|row| row.fade[2] as f64 + row.fade[3] as f64).reduce(f64::max);
        self.leaders.set(templates.line.slots(), leaders);
        self.minis.set(templates.mini.slots(), minis);
        self.pins.set(templates.pin.slots(), pins);
        let b = self.data.frame.map(|x| x as f32);
        let mut c = Common::at([0.0; 3], tokens.block_frame);
        self.backdrop_fade.apply(&mut c);
        // Opaque: inside the panel only the reveal shows, never the ghosted sheet behind it.
        let fill = tokens.block_fill;
        let mut rows = Vec::with_capacity(templates.boxes.slots());
        templates.boxes.append(&mut rows, &BoxInst {
            c, rect: [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5, (b[2] - b[0]) * 0.5, (b[3] - b[1]) * 0.5],
            corner: 1.0, stroke: [1.0, 0.0], fill,
        });
        self.backdrop.set(templates.boxes.slots(), rows);
    }

    pub(super) fn home(&mut self, sheet: &SheetLayout, tokens: &Tokens, templates: &Templates, now: f64) {
        if self.home_end.is_some() { return; }
        let detail_alpha = self.state.fly.as_ref().map_or(1.0, |fly| fly.detail.pose(now)[3]);
        let mut parts = vec![None; self.display.parts.len()];
        let mut end = now + STATE_SECONDS;
        for (k, (n, p)) in self.display.parts.iter().enumerate().filter(|(_, p)| p.placed).enumerate() {
            let current = self.scene.position(n as u32, now).unwrap_or(p.at.map(f64::from));
            let target = sheet.parts.get(n).filter(|p| p.placed).map_or(p.at, |p| p.at);
            let motion = Flight {
                from: [(current[0] - p.at[0] as f64) as f32, (current[1] - p.at[1] as f64) as f32, 0.0, 1.0],
                to: [(target[0] - p.at[0]) as f32, (target[1] - p.at[1]) as f32, 0.0, 1.0],
                span: [(now + STATE_SECONDS + 0.01 * (k % 24) as f64) as f32, REVEAL_SECONDS as f32],
            };
            end = end.max(motion.end());
            parts[n] = Some(motion);
        }
        self.state.fly = Some(FlyFrom { parts, detail: fade(detail_alpha, 0.0, now, STATE_SECONDS),
            leg: Leg::Leave, start: now, continued: false, text_from: Vec::new() });
        self.backdrop_fade = fade(self.backdrop_fade.pose(now)[3], 0.0, now, STATE_SECONDS);
        self.ghost = fade(self.sheet_alpha(now), 1.0, end - STATE_SECONDS, STATE_SECONDS);
        self.home_end = Some(end.max(self.ghost.end()));
        self.trim_at = None;
        self.trail.clear();
        self.chips.set(Vec::new(), now);
        self.trail_origin = None;
        self.rebuild(tokens, templates);
    }

    /// Discard returning extras once, at their landing; never republish rows for a clock tick.
    pub(super) fn settle(&mut self, tokens: &Tokens, templates: &Templates, now: f64) {
        if self.trim_at.is_some_and(|t| now >= t) {
            self.trim_at = None;
            self.display = self.data.sheet.clone();
            self.rebuild(tokens, templates);
        }
        if self.bridges_end.is_some_and(|t| now >= t) {
            self.bridges_end = None;
            self.bridges.set(templates.bridge.slots(), Vec::new());
        }
    }

    pub fn sheet(&self) -> &SheetLayout { &self.display }
    pub fn design(&self) -> &Arc<Design> { &self.design }
    pub fn sheet_alpha(&self, now: f64) -> f32 { self.ghost.pose(now)[3] }
    pub fn finished(&self, now: f64) -> bool {
        self.home_end.is_some_and(|end| now >= end.max(self.scene.settle_at()))
    }
    pub fn animating(&self, now: f64) -> bool {
        now < self.scene.settle_at() || now < self.ghost.end() || now < self.backdrop_fade.end() || self.chips.animating(now)
    }

    pub fn trail_hit(&self, screen: [f64; 2]) -> Option<u32> {
        let id = self.chips.hit(screen)?;
        self.trail.iter().find(|entry| entry.id == id).map(|entry| entry.focus)
    }

    pub fn draw_layers(&mut self, cx: &mut Cx2d, uniforms: &CanvasUniforms, cam: &Camera2d, tokens: &Tokens,
        live: &CanvasLive, boxes: &mut DrawCanvasBox, lines: &mut DrawCanvasLine, arcs: &mut DrawCanvasArc,
        bridges: &mut DrawCanvasBridge, minis: &mut DrawCanvasMini, pins: &mut DrawCanvasPin, mini_hover: f32)
    {
        uniforms.with(tokens, |u| {
            self.backdrop.draw(cx, &mut boxes.draw_vars, u, live);
            self.boxes.draw(cx, &mut boxes.draw_vars, u, live);
            self.lines.draw(cx, &mut lines.draw_vars, u, live);
            self.arcs.draw(cx, &mut arcs.draw_vars, u, live);
            self.bridges.draw(cx, &mut bridges.draw_vars, u, live);
            self.pins.draw(cx, &mut pins.draw_vars, u, live);
            if text_points(fab_elec_explore::sheet::TEXT_NET, cam.px_per_unit) >= TEXT_MIN_POINTS {
                self.leaders.draw(cx, &mut lines.draw_vars, u, live);
            }
        });
        let mini_uniforms = CanvasUniforms { layer_alpha: uniforms.layer_alpha * mini_alpha(cam.px_per_unit), ..*uniforms };
        draw_mini_layer(cx, &mut self.minis, minis, &mini_uniforms, tokens, live, mini_hover);
    }

    pub fn draw_trail(&mut self, cx: &mut Cx2d, cam: &Camera2d, uniforms: &CanvasUniforms, tokens: &Tokens,
        live: &CanvasLive, boxes: &mut DrawCanvasBox, lines: &mut DrawCanvasLine, text: &mut DrawText, now: f64)
    {
        let origin = [cam.view.pos.x + 12.0, cam.view.pos.y + 12.0];
        if self.trail_origin != Some(origin) {
            self.trail_origin = Some(origin);
            let old_size = text.text_style.font_size;
            let old_scale = text.font_scale;
            text.text_style.font_size = 11.0;
            text.font_scale = 1.0;
            let mut x = origin[0];
            let chips = self.trail.iter().map(|entry| {
                let width = text.layout(cx, 0.0, 0.0, None, false, Align::default(), &entry.text).size_in_lpxs.width as f64;
                let chip = Chip { id: entry.id, kind: ChipKind::Trail, text: entry.text.clone(),
                    anchor: ChipAnchor::Screen { at: [x, origin[1]] }, align: [0.0; 2] };
                x += width + 20.0;
                chip
            }).collect();
            text.text_style.font_size = old_size;
            text.font_scale = old_scale;
            self.chips.set(chips, now);
        }
        self.chips.draw(cx, cam, uniforms.clip, tokens, live, lines, boxes, text, now);
    }
}

/// One pick order and one semantic node per item, including during a flight.
pub struct ExplorerItems<'a> {
    pub sheet: &'a Scene,
    pub reveal: Option<&'a Reveal>,
    pub cam: &'a Camera2d,
    pub now: f64,
}

impl ExplorerItems<'_> {
    /// Every part the explorer shows, the reveal's first (a part the reveal also places is
    /// its). O(parts), once per query or action.
    pub fn lookup_parts(&self) -> Vec<LookupPart> {
        let mut out = Vec::new();
        let overlay = self.reveal.map(|reveal| &reveal.scene);
        if let Some(scene) = overlay {
            scene.lookup_parts(self.now, None, &mut out);
        }
        self.sheet.lookup_parts(self.now, overlay, &mut out);
        out
    }

    /// A rectangle (window points) whose centre the pointer picks as `sel` first: around the
    /// drawn body's centre, else one of its quarters' centres. None when the body is out of
    /// view or covered there. The serialized f32 centre is the one tested, so rounding cannot
    /// hand a press to a neighbour.
    pub fn press_rect(&self, sel: &Sel, body: [f64; 4]) -> Option<a11y_tree::Rect> {
        let cam = self.cam;
        if !cam.px_per_unit.is_finite() || cam.px_per_unit <= 0.0 {
            return None;
        }
        let [x0, y0, x1, y1] = body;
        let (cx, cy) = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
        let points = [[cx, cy], [(x0 + cx) * 0.5, (y0 + cy) * 0.5], [(cx + x1) * 0.5, (y0 + cy) * 0.5],
            [(x0 + cx) * 0.5, (cy + y1) * 0.5], [(cx + x1) * 0.5, (cy + y1) * 0.5]];
        points.into_iter().find_map(|world| {
            let at = cam.to_screen(world);
            let rect = makepad_platform::a11y::semantic_rect(Rect {
                pos: at - dvec2(HIT_RADIUS, HIT_RADIUS), size: dvec2(2.0 * HIT_RADIUS, 2.0 * HIT_RADIUS),
            });
            let (x, y) = rect.center();
            let at = dvec2(f64::from(x), f64::from(y));
            (cam.view.contains(at) && self.hit(cam.to_world(at), HIT_RADIUS / cam.px_per_unit).first() == Some(sel))
                .then_some(rect)
        })
    }

    /// The parts matching a lookup, as (stable part key, node), in natural refdes order.
    /// `key` is one part's stable key (an id term resolved by the platform, or a `Key` lookup).
    /// A part out of view or covered has no bounds and `OFFSCREEN`; every part offers
    /// `SCROLL_INTO_VIEW`. O(parts) per query.
    pub fn lookup(&self, query: Option<&a11y_tree::query::Query>, key: Option<u64>) -> Vec<(u64, a11y_tree::Node)> {
        if query.is_some_and(|q| !q.is_lookup()) || (query.is_some_and(|q| q.id_term().is_some()) && key.is_none()) {
            // Structural filters alone materialise nothing; a stale or foreign id scans nothing.
            return Vec::new();
        }
        let name = query.and_then(|q| q.name_term());
        let text = query.and_then(|q| q.text_term());
        let mut parts = self.lookup_parts();
        parts.retain(|part| {
            key.is_none_or(|key| part_key(&part.sel) == key)
                && name.is_none_or(|(term, exact)| if exact { part.name == term } else { part.name.to_lowercase().contains(term) })
                // Text discovers names; value terms are checked on the built node.
                && text.is_none_or(|term| part.name.to_lowercase().contains(term))
        });
        parts.sort_by(|a, b| natural_cmp(&a.name, &b.name));
        parts.into_iter().filter_map(|part| {
            let bounds = part.body.and_then(|body| self.press_rect(&part.sel, body));
            let (key, mut node) = node_part(&CanvasNode {
                sel: part.sel, rect: Rect::default(), role: "part", name: part.name, value: part.value,
            });
            node.actions.insert(a11y_tree::Actions::SCROLL_INTO_VIEW);
            match bounds {
                Some(bounds) => node.bounds = bounds,
                None => {
                    node.bounds = Default::default();
                    node.states.insert(a11y_tree::States::OFFSCREEN);
                }
            }
            if let Some(q) = query {
                // Wire ids belong to the platform: supplied only while matching.
                node.id = q.id_term().unwrap_or_default();
                if !q.matches(&node) {
                    return None;
                }
                node.id = a11y_tree::NodeId::NONE;
            }
            Some((key, node))
        }).collect()
    }
}

impl CanvasItems for ExplorerItems<'_> {
    type Key = Sel;

    fn hit(&self, at: [f64; 2], radius: f64) -> Vec<Sel> {
        let mut hits = Vec::new();
        if let Some(reveal) = self.reveal {
            let screen = self.cam.to_screen(at);
            if let Some(sel) = reveal.trail_hit([screen.x, screen.y])
                .and_then(|node| reveal.rows.comp(node))
                .and_then(|item| reveal.scene.sel_of(item))
            {
                hits.push(sel);
                return hits;
            }
            reveal.scene.hit_into(at, radius, self.now, &mut hits);
        }
        self.sheet.hit_into(at, radius, self.now, &mut hits);
        hits
    }

    fn nodes(&self, cam: &Camera2d) -> Vec<CanvasNode<Sel>> {
        let mut nodes = Vec::new();
        let overlay = self.reveal.map(|r| &r.scene);
        if let Some(scene) = overlay { scene.nodes_into(cam, self.now, None, &mut nodes); }
        self.sheet.nodes_into(cam, self.now, overlay, &mut nodes);
        nodes
    }
}

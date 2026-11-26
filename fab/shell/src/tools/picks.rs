//! Picks: what the person points at in the Edit shape mode (E), for the AI. A click picks what is
//! under the pointer, Shift adds or takes off, a double click picks the whole body; holding the
//! press still opens Select Other, everything along the pointer's ray. The kind filter (Any,
//! Face, Edge, Feature, Body) says what a click takes; on a part drawn from its field a face is
//! the edit there and an edge a crease or an edit's own edges. The model worker resolves and
//! describes each pick (`fab.select`); the view keeps what it draws: a marker at each pick's spot
//! (a box for a pick named without one), an exact part's face or edge outlined, what a click
//! would take at the pointer (its words, and its outline on an exact part), and Select Other's
//! list. Every pick is also a chip on the chat's composer.
//!
//! Hold the right button in Edit shape mode for a ring of commands that fit the picks. A stroke
//! toward a command runs it on release. Releasing in the hub leaves the ring open for a click.
//! A held press starts one timer. While open, a pointer move checks the pills and one sector.
//!
//! On a part drawn from its field the edits picked, and the one a click would take, are tinted
//! where the part draws them (each tile holds its edit's ordinal; a mask row per part says how
//! strongly), and a crease under the pointer gets a tick along it. An exact part's outlines draw
//! in the viewport against its G-buffer: what is behind a part dimmed and dashed.
//!
//! Cost: at rest nothing; a pointer move in E sends one ray to the worker (the app keeps one in
//! flight, the newest waiting); while picks exist, a marker each in the tool overlay, their
//! outlines' points carried to the world each frame (geometry made again only when they move),
//! and per field part they tint one mask compare; a changed tint writes one mask row.

use crate::api::*;
use crate::api::PaneId;
use crate::ui::command_palette::COMMANDS;
use makepad_widgets_core::*;
use std::f64::consts::{FRAC_1_SQRT_2 as DIAGONAL, FRAC_PI_4};
use std::sync::Arc;

type V3 = [f64; 3];

/// One pick as the view draws it.
#[derive(Clone, Debug, PartialEq)]
pub struct PickView {
    /// The worker's name for it (`p3`): the chat's chip says it back.
    pub id: String,
    /// edit, crease, edges, spot, feature or body.
    pub kind: String,
    pub label: String,
    /// The printed object it is on (`instance/object`).
    pub object: String,
    /// Where it was pointed at, in the object's model frame: point and outward normal.
    pub at: Option<(V3, V3)>,
    /// Its solid's box in that frame.
    pub bounds: Option<(V3, V3)>,
    /// An exact part's face outline or edge curves, in that frame.
    pub outline: Arc<Vec<Vec<V3>>>,
    /// On a part drawn from its field, the features it lights up and how strongly
    /// (`csg_sdf::SdfNode::Id` ids): an edit's, a crease's two at half.
    pub tint: Vec<(u32, f32)>,
}

/// What a click picks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickKind {
    /// As it lands: on an exact part the edge near the pointer, else the face; on a part drawn
    /// from its field a crease near where two edits meet, an edit's edges near its own edge, else
    /// the edit; on any other the feature.
    #[default]
    Any,
    /// An exact part's face; the edit on a part drawn from its field.
    Face,
    /// An exact part's edge; a crease or an edit's own edges on a part drawn from its field.
    Edge,
    Feature,
    Body,
}

impl PickKind {
    pub const ALL: [PickKind; 5] = [PickKind::Any, PickKind::Face, PickKind::Edge, PickKind::Feature, PickKind::Body];

    /// As the worker takes it.
    pub fn name(self) -> &'static str {
        match self {
            PickKind::Any => "any",
            PickKind::Face => "face",
            PickKind::Edge => "edge",
            PickKind::Feature => "feature",
            PickKind::Body => "body",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            PickKind::Any => "Any",
            PickKind::Face => "Face",
            PickKind::Edge => "Edge",
            PickKind::Feature => "Feature",
            PickKind::Body => "Body",
        }
    }
}

/// One row of Select Other.
#[derive(Clone, Debug, PartialEq)]
pub struct OtherRow {
    pub kind: String,
    pub label: String,
    /// The pick as `fab.select`'s `add` takes it (JSON).
    pub pick: String,
}

/// Select Other's list, at the pointer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtherList {
    pub at: DVec2,
    pub rows: Vec<OtherRow>,
    pub hover: Option<usize>,
    /// The press that opened it was with Shift: a row adds.
    pub add: bool,
}

/// A command the person can run on compatible picks from the strip or the marking menu.
pub struct PickCommand {
    /// The string handed to the app's command runner.
    pub command: &'static str,
    /// The short name drawn on the strip and the ring.
    pub label: &'static str,
    /// The command's fixed direction on the ring, clockwise from north.
    pub slot: u8,
    /// The pick kinds this command takes. An empty slice accepts any pick.
    pub kinds: &'static [&'static str],
    /// Whether to offer this command on the strip.
    pub strip: bool,
}

impl PickCommand {
    fn takes(&self, picks: &[PickView]) -> bool {
        picks.iter().any(|p| self.kinds.is_empty() || self.kinds.contains(&p.kind.as_str()))
    }
}

/// The pick commands in the order the strip offers them.
pub const PICK_COMMANDS: &[PickCommand] = &[
    PickCommand { command: "round", label: "Round", slot: 2, kinds: &["edge", "chain", "crease", "edges"], strip: true },
    PickCommand { command: "chamfer", label: "Chamfer", slot: 3, kinds: &["edge", "chain"], strip: true },
    PickCommand { command: "pull", label: "Pull", slot: 1, kinds: &["face"], strip: true },
    PickCommand { command: "hole", label: "Hole", slot: 6, kinds: &["face"], strip: true },
    PickCommand { command: "shell", label: "Shell", slot: 5, kinds: &["face"], strip: true },
    PickCommand { command: "insert selector", label: "Insert Selector", slot: 4, kinds: &[], strip: false },
];

/// The person's last committed pick command, for Repeat Last.
#[derive(Clone, Debug)]
pub struct Repeat {
    /// The command to plan on the current picks.
    pub command: String,
    /// The value the person committed, in millimetres.
    pub value: f64,
    /// The command and its value as shown to the person.
    pub words: String,
}

/// One command offered in the open ring and the chip that follows the pointer.
#[derive(Clone, Debug)]
pub struct MenuItem {
    pub disabled: bool,
    /// The compass slot the command occupies.
    pub slot: u8,
    /// The name on the pill.
    pub label: String,
    /// The command sent when the person releases on this item.
    pub command: &'static str,
    /// The words on the pointer's chip.
    pub words: String,
}

/// The marking menu from the opening press until a command runs or the person dismisses it.
#[derive(Clone, Debug)]
pub struct MarkingMenu {
    /// The opening press, in window points. Strokes keep this origin even if the pills move.
    pub at: DVec2,
    /// Where the pointer and its chip are now, in window points.
    pub pointer: DVec2,
    /// The commands offered for the picks when the menu opened.
    pub items: Vec<MenuItem>,
    /// The slot lit up under the pointer or in the stroke's direction.
    pub hot: Option<u8>,
    /// Whether the opening right press is still held.
    pub stroke: bool,
    /// Whether a click-mode press is waiting for its release.
    pub press: bool,
    /// The Plate view that received the opening press.
    pub(crate) pane: PaneId,
    view: Rect,
    /// The same pill bounds for drawing, hit testing and the semantic tree.
    pub(super) layout: Arc<MenuLayout>,
}

impl MarkingMenu {
    /// Open around the right press with the commands offered for these picks.
    pub(super) fn open(pane: PaneId, view: Rect, at: DVec2, items: Vec<MenuItem>) -> Self {
        let layout = Arc::new(menu_layout(view, at, &items));
        Self { pane, view, layout, at, pointer: at, items, hot: None, stroke: true, press: false }
    }

    /// Rewrap the pills if the view moved or resized while the ring was open.
    pub(super) fn fit(&mut self, view: Rect) {
        if self.view != view {
            self.view = view;
            self.layout = Arc::new(menu_layout(view, self.at, &self.items));
            self.hot = menu_hot(self, &self.layout, self.pointer);
        }
    }
}

/// The ring's fitted centre and the pills as they draw in window points.
#[derive(Debug)]
pub struct MenuLayout {
    /// The centre of the fitted ring, which can shift inward near an edge.
    pub centre: DVec2,
    /// Each visible pill's compass slot, bounds and wrapped label.
    pub pills: Vec<(u8, Rect, Vec<String>)>,
}

/// The picks and what the view shows of them.
#[derive(Clone, Debug, Default)]
pub struct PickState {
    pub list: Arc<Vec<PickView>>,
    /// The worker's revision of `list`.
    pub rev: u64,
    pub filter: PickKind,
    /// The pick lit up: its chat chip under the pointer.
    pub lit: Option<String>,
    /// What a click under the pointer would pick, in words, at the pointer.
    pub under: Option<Under>,
    /// Select Other, while it is open.
    pub other: Option<OtherList>,
    /// The marking menu, while it is open.
    pub menu: Option<MarkingMenu>,
    /// The last command the person committed on a pick.
    pub repeat: Option<Repeat>,
}

/// Where the pointer rests: its ray (plate mm), plate mm per pixel at the eye and per mm of
/// depth, the window point, and the view it rests on (camera and rect).
#[derive(Clone, Copy, Debug)]
pub struct Hover {
    pub scene: SceneHit,
    pub from: V3,
    pub dir: V3,
    pub pixel: [f64; 2],
    pub at: DVec2,
    pub view: (Camera, Rect),
}

/// What a click at the pointer would pick, in words. It speaks for the view it was asked in:
/// once the camera or the view's rect moves (a panel opens, the camera glides), it is not shown
/// until the pointer asks again.
#[derive(Clone, Debug)]
pub struct Under {
    pub at: DVec2,
    pub words: String,
    pub view: (Camera, Rect),
    /// On an exact part: the printed object, and the outline of what a click would take there
    /// (model frame).
    pub outline: Option<(String, Vec<Vec<V3>>)>,
    /// The printed object it is on.
    pub object: String,
    /// On a part drawn from its field: as [`PickView::tint`]; on a crease, its point and way
    /// along it (model frame).
    pub tint: Vec<(u32, f32)>,
    pub tick: Option<(V3, V3)>,
}

/// A hit (or miss) on the scene installed when the pointer event arrived.
#[derive(Clone, Copy, Debug)]
pub struct SceneHit {
    pub revision: u64,
    pub hit: Option<crate::model::RayHit>,
}

/// What the view asks of the app (which asks the model worker).
#[derive(Clone, Debug)]
pub enum PickAction {
    /// A click: the pointer's ray (plate mm), plate mm per pixel at the eye and per mm of depth,
    /// Shift (adds, or takes off what is picked), a double click (the body).
    At { scene: SceneHit, from: V3, dir: V3, pixel: [f64; 2], add: bool, body: bool },
    /// The pointer rests; None: it left.
    Hover(Option<Hover>),
    /// Press and hold: Select Other at `at`.
    Other { scene: SceneHit, from: V3, dir: V3, pixel: [f64; 2], at: DVec2, add: bool },
    /// Select Other's row `n` was chosen; None: it closed.
    Row(Option<usize>),
    /// A kind for the filter.
    Filter(PickKind),
}

/// Plate mm per pixel of a view: at the eye, and per mm of depth.
pub fn pixel(cam: &Camera, rect: Rect) -> [f64; 2] {
    let h = rect.size.y.max(1.0);
    if cam.ortho {
        [cam.ortho_height as f64 * 1000.0 / h, 0.0]
    } else {
        [0.0, 2.0 * ((cam.fov_y_deg as f64).to_radians() * 0.5).tan() / h]
    }
}

/// The pick controls share the bottom of the lead viewport with the marked step's status bar.
pub fn strip_view(state: &AppState, pane: PaneId, mut view: Rect) -> Rect {
    if state.steps.shown.is_some() && pane == state.view.lead_plate() {
        view.size.y -= 32.0;
    }
    view
}

/// The kind filter's strip in the view while picks are made: each kind's box (window points)
/// along the view's bottom left, right of the tool rail and the Edit shape strip.
pub fn strip(view: Rect) -> Vec<(Rect, PickKind)> {
    let widths = PickKind::ALL.iter().map(|k| pill_width(k.label().chars().count()));
    flow(view, view.pos.x + 104.0, view.pos.y + view.size.y - 36.0, widths).into_iter().zip(PickKind::ALL.iter().copied()).collect()
}

/// Boxes of `widths` laid left to right from `x` on the row at `y`, a row up where one would
/// pass the view's right edge: the strip wraps as the view narrows.
fn flow(view: Rect, mut x: f64, mut y: f64, widths: impl Iterator<Item = f64>) -> Vec<Rect> {
    let left = view.pos.x + 104.0;
    let right = view.pos.x + view.size.x - 8.0;
    widths
        .map(|w| {
            if x + w > right && x > left {
                x = left;
                y -= 26.0;
            }
            let r = Rect { pos: dvec2(x, y), size: dvec2(w, 22.0) };
            x += w + 4.0;
            r
        })
        .collect()
}

/// Whether `pos` is on the kind filter's strip or a command's box: a press there is theirs, not
/// the view's.
pub fn on_strip(view: Rect, picks: &[PickView], pos: DVec2) -> bool {
    strip(view).iter().any(|(r, _)| r.contains(pos)) || commands(view, picks).iter().any(|(r, ..)| r.contains(pos))
}

/// The commands that act on the picks there are, right of the kind filter's strip: each one's box
/// and its palette command. Round for an exact part's edges or chains, a crease or an edit's
/// edges; Chamfer for exact edges alone; Pull, Hole and Shell for an exact part's faces.
pub fn commands(view: Rect, picks: &[PickView]) -> Vec<(Rect, &'static str, &'static str)> {
    let (x, y) = strip(view).last().map_or((view.pos.x + 104.0, view.pos.y + view.size.y - 36.0), |(r, _)| (r.pos.x + r.size.x + 16.0, r.pos.y));
    let on: Vec<_> = PICK_COMMANDS.iter().filter(|entry| entry.strip && entry.takes(picks)).collect();
    flow(view, x, y, on.iter().map(|entry| pill_width(entry.label.chars().count())))
        .into_iter().zip(on).map(|(r, entry)| (r, entry.label, entry.command)).collect()
}

pub fn modelling_command(name: &str) -> bool {
    name == "repeat last" || PICK_COMMANDS.iter().any(|entry| entry.strip && entry.command == name)
}

/// The ring commands that take the current picks, including a compatible Repeat Last.
pub fn menu_items(picks: &[PickView], repeat: Option<&Repeat>) -> Vec<MenuItem> {
    let mut items = Vec::new();
    for entry in PICK_COMMANDS.iter().filter(|entry| entry.takes(picks)) {
        let (_, words, _) = COMMANDS.iter().find(|row| row.0 == entry.command).expect("a pick command has a palette label");
        items.push(MenuItem { disabled: false, slot: entry.slot, label: entry.label.into(), command: entry.command, words: (*words).into() });
    }
    if let Some(repeat) = repeat.filter(|r| items.iter().any(|item| item.command == r.command)) {
        items.insert(0, MenuItem {
            disabled: false, slot: 0, label: format!("Repeat {}", repeat.words), command: "repeat last",
            words: format!("Repeat {} on the picks", repeat.words),
        });
    }
    items
}

/// The radius of the drawn hub and the stroke's dead zone, in window points.
pub(super) const HUB_RADIUS: f64 = 14.0;
const DIRECTIONS: [DVec2; 8] = [
    dvec2(0.0, -1.0), dvec2(DIAGONAL, -DIAGONAL), dvec2(1.0, 0.0), dvec2(DIAGONAL, DIAGONAL),
    dvec2(0.0, 1.0), dvec2(-DIAGONAL, DIAGONAL), dvec2(-1.0, 0.0), dvec2(-DIAGONAL, -DIAGONAL),
];

fn pill_width(chars: usize) -> f64 {
    chars as f64 * 6.5 + 18.0
}

/// Wrap only at spaces. A word that cannot fit asks the caller to try a smaller ring.
pub(super) fn wrap_pill(text: &str, room: f64) -> Option<(Vec<String>, DVec2)> {
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut used = 0;
    let mut longest = 0;
    for word in text.split_whitespace() {
        let chars = word.chars().count();
        if pill_width(chars) > room {
            return None;
        }
        if used > 0 {
            if pill_width(used + 1 + chars) > room {
                lines.push(std::mem::take(&mut line));
                used = 0;
            } else {
                line.push(' ');
                used += 1;
            }
        }
        line.push_str(word);
        used += chars;
        longest = longest.max(used);
    }
    lines.push(line);
    let size = dvec2(pill_width(longest), lines.len() as f64 * 14.0 + 8.0);
    Some((lines, size))
}

/// Attach the inward-facing edge of the pill to the ring.
fn pill_rect(anchor: DVec2, dir: DVec2, size: DVec2) -> Rect {
    let offset = if dir.x > 0.3 {
        dvec2(0.0, size.y * 0.5)
    } else if dir.x < -0.3 {
        dvec2(size.x, size.y * 0.5)
    } else {
        dvec2(size.x * 0.5, if dir.y < 0.0 { size.y } else { 0.0 })
    };
    Rect { pos: anchor - offset, size }
}

fn ring(bounds: Rect, mut centre: DVec2, items: &[MenuItem], radius: f64, move_inward: bool) -> Option<MenuLayout> {
    let end = bounds.pos + bounds.size;
    if move_inward {
        // Each label needs at least its longest word on its side. Move only as far as needed.
        let mut left = bounds.pos.x + radius;
        let mut right = end.x - radius;
        for item in items {
            let dir = DIRECTIONS[item.slot as usize];
            let width = pill_width(item.label.split_whitespace().map(|word| word.chars().count()).max().unwrap_or(0));
            let pill = pill_rect(dir * radius, dir, dvec2(width, 0.0));
            left = left.max(bounds.pos.x - pill.pos.x);
            right = right.min(end.x - (pill.pos.x + width));
        }
        if left > right {
            return None;
        }
        centre.x = centre.x.clamp(left, right);
    }

    let mut pills = Vec::with_capacity(items.len());
    let mut top = -radius;
    let mut bottom = radius;
    for item in items {
        let dir = DIRECTIONS[item.slot as usize];
        let offset = dir * radius;
        let anchor_x = centre.x + offset.x;
        let left = anchor_x - bounds.pos.x;
        let right = end.x - anchor_x;
        let room = if dir.x > 0.3 { right } else if dir.x < -0.3 { left } else { 2.0 * left.min(right) };
        let (lines, size) = wrap_pill(&item.label, room)?;
        // Wrapped heights determine how far the whole ring reaches above and below the centre.
        let rect = pill_rect(offset, dir, size);
        top = top.min(rect.pos.y);
        bottom = bottom.max(rect.pos.y + size.y);
        pills.push((item.slot, rect, lines));
    }
    if move_inward && bottom - top <= bounds.size.y {
        centre.y = centre.y.max(bounds.pos.y - top).min(end.y - bottom);
    }
    for (_, rect, _) in &mut pills {
        *rect = bounds.contain(rect.translate(centre));
        if !rect.is_inside_of(bounds) {
            return None;
        }
        let nearest = dvec2(centre.x.clamp(rect.pos.x, rect.pos.x + rect.size.x), centre.y.clamp(rect.pos.y, rect.pos.y + rect.size.y));
        if centre.distance(&nearest) <= HUB_RADIUS {
            return None;
        }
    }
    for (i, (_, rect, _)) in pills.iter().enumerate() {
        if pills[..i].iter().any(|(_, other, _)| rect.intersects(*other)) {
            return None;
        }
    }
    Some(MenuLayout { centre, pills })
}

/// Fit the offered pills around the press, moving the smallest ring inward when needed.
pub fn menu_layout(view: Rect, at: DVec2, items: &[MenuItem]) -> MenuLayout {
    let bounds = view.add_margin(dvec2(-8.0, -8.0));
    for (radius, move_inward) in [(72.0, false), (56.0, false), (40.0, false), (40.0, true)] {
        if let Some(layout) = ring(bounds, at, items, radius, move_inward) {
            return layout;
        }
    }
    // A view too small for the pills still has all its stroke directions.
    MenuLayout { centre: at, pills: Vec::new() }
}

/// Where the hot command's words show, and their lines: beside its pill on the side away from
/// the hub (right of a right-hand pill, left of a left-hand one, above north, below south), else
/// below or above the pill, else below or above the whole ring: the first place inside the view
/// that covers no pill and not the hub. With no pills drawn (a view too small for them) the words ride the pointer.
pub fn chip_rect(view: Rect, layout: &MenuLayout, slot: u8, pointer: DVec2, words: &str) -> Option<(Rect, Vec<String>)> {
    const GAP: f64 = 6.0;
    let bounds = view.add_margin(dvec2(-8.0, -8.0));
    let end = bounds.pos + bounds.size;
    let Some((_, pill, _)) = layout.pills.iter().find(|(s, ..)| *s == slot) else {
        let pos = pointer + dvec2(16.0, 14.0);
        let (lines, size) = wrap_pill(words, (end.x - pos.x).max(bounds.size.x.min(160.0)))?;
        return Some((bounds.contain(Rect { pos, size }), lines));
    };
    #[derive(Clone, Copy)]
    enum Side { Right, Left, Below, Above, UnderRing, OverRing }
    let dir = DIRECTIONS[slot as usize];
    let order = if dir.x > 0.3 {
        [Side::Right, Side::Below, Side::Above, Side::UnderRing, Side::OverRing]
    } else if dir.x < -0.3 {
        [Side::Left, Side::Below, Side::Above, Side::UnderRing, Side::OverRing]
    } else if dir.y < 0.0 {
        [Side::Above, Side::Right, Side::Left, Side::OverRing, Side::UnderRing]
    } else {
        [Side::Below, Side::Right, Side::Left, Side::UnderRing, Side::OverRing]
    };
    let ring_top = layout.pills.iter().map(|(_, p, _)| p.pos.y).fold(layout.centre.y - HUB_RADIUS, f64::min);
    let ring_bottom = layout.pills.iter().map(|(_, p, _)| p.pos.y + p.size.y).fold(layout.centre.y + HUB_RADIUS, f64::max);
    let hub = Rect { pos: layout.centre, size: DVec2::default() }.add_margin(DVec2::all(HUB_RADIUS));
    let mut first = None;
    for side in order {
        let room = match side {
            Side::Right => end.x - (pill.pos.x + pill.size.x) - GAP,
            Side::Left => pill.pos.x - GAP - bounds.pos.x,
            Side::Below | Side::Above | Side::UnderRing | Side::OverRing => bounds.size.x,
        };
        let Some((lines, size)) = wrap_pill(words, room) else { continue };
        let pos = match side {
            Side::Right => dvec2(pill.pos.x + pill.size.x + GAP, pill.center().y - size.y * 0.5),
            Side::Left => dvec2(pill.pos.x - GAP - size.x, pill.center().y - size.y * 0.5),
            Side::Below => dvec2(pill.center().x - size.x * 0.5, pill.pos.y + pill.size.y + GAP),
            Side::Above => dvec2(pill.center().x - size.x * 0.5, pill.pos.y - GAP - size.y),
            Side::UnderRing => dvec2(pill.center().x - size.x * 0.5, ring_bottom + GAP),
            Side::OverRing => dvec2(pill.center().x - size.x * 0.5, ring_top - GAP - size.y),
        };
        let rect = bounds.contain(Rect { pos, size });
        if rect.is_inside_of(bounds) && !rect.intersects(hub) && layout.pills.iter().all(|(_, p, _)| !rect.intersects(*p)) {
            return Some((rect, lines));
        }
        first.get_or_insert((rect, lines));
    }
    first
}

/// The pill under the pointer or, beyond the hub, the offered slot in the stroke's direction.
pub fn menu_hot(menu: &MarkingMenu, layout: &MenuLayout, pointer: DVec2) -> Option<u8> {
    if let Some((slot, ..)) = layout.pills.iter().find(|(_, rect, _)| rect.contains(pointer)) {
        return Some(*slot);
    }
    let stroke = pointer - menu.at;
    if stroke.length() <= HUB_RADIUS {
        return None;
    }
    let slot = ((stroke.x.atan2(-stroke.y) / FRAC_PI_4).round() as i32).rem_euclid(8) as u8;
    menu.items.iter().find(|item| item.slot == slot).map(|item| item.slot)
}

/// Picks are made in the Edit shape mode of the Select tool, in Design.
pub fn active(state: &AppState) -> bool {
    state.ui.gizmo == GizmoMode::Shape && state.tool == Tool::Select && state.ui.in_mode(PlateMode::Design)
}

/// A model point of part `object` on the plate (mm), when the part is there.
pub fn on_plate(state: &AppState, object: &str, p: V3) -> Option<V3> {
    let part = state.plate.parts.iter().find(|part| part.object == object)?;
    let m = invert(&part.model_from_printer)?;
    Some(xf(&m, p))
}

fn xf(m: &[[f64; 4]; 3], p: V3) -> V3 {
    std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
}

/// The inverse of an affine map (row-major 3x4).
fn invert(m: &[[f64; 4]; 3]) -> Option<[[f64; 4]; 3]> {
    let a = |r: usize, c: usize| m[r][c];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0)) + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if det.abs() < 1e-15 {
        return None;
    }
    let inv = [
        [(a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) / det, (a(0, 2) * a(2, 1) - a(0, 1) * a(2, 2)) / det, (a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1)) / det],
        [(a(1, 2) * a(2, 0) - a(1, 0) * a(2, 2)) / det, (a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0)) / det, (a(0, 2) * a(1, 0) - a(0, 0) * a(1, 2)) / det],
        [(a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0)) / det, (a(0, 1) * a(2, 0) - a(0, 0) * a(2, 1)) / det, (a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0)) / det],
    ];
    let t = [m[0][3], m[1][3], m[2][3]];
    Some(std::array::from_fn(|r| [inv[r][0], inv[r][1], inv[r][2], -(inv[r][0] * t[0] + inv[r][1] * t[1] + inv[r][2] * t[2])]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marking_menu_lays_out_and_strokes() {
        let pick = |kind: &str| PickView {
            id: String::new(), kind: kind.into(), label: String::new(), object: String::new(),
            at: None, bounds: None, outline: Arc::default(), tint: Vec::new(),
        };
        let slots = |items: &[MenuItem]| {
            let mut slots: Vec<_> = items.iter().map(|item| (item.slot, item.command)).collect();
            slots.sort_unstable();
            slots
        };
        let repeat = Repeat { command: "round".into(), value: 2.0, words: "Round 2 mm".into() };
        let edge = menu_items(&[pick("edge")], None);
        assert_eq!(slots(&edge), [(2, "round"), (3, "chamfer"), (4, "insert selector")],
            "an exact edge offers both edge commands and Insert Selector in their fixed slots");
        let face = menu_items(&[pick("face")], Some(&repeat));
        assert_eq!(slots(&face), [(1, "pull"), (4, "insert selector"), (5, "shell"), (6, "hole")],
            "a face offers the three face commands and Insert Selector, excluding an edge repeat");
        let repeated = menu_items(&[pick("edge")], Some(&repeat));
        let north = repeated.iter().find(|item| item.slot == 0).expect("a compatible repeat occupies north");
        assert_eq!((north.command, north.label.as_str(), north.words.as_str()),
            ("repeat last", "Repeat Round 2 mm", "Repeat Round 2 mm on the picks"),
            "Repeat names the last operation and value on the pill and chip");
        assert_eq!(face.iter().find(|item| item.slot == 6).unwrap().words, "Drill a Hole at the Picked Face", "the Hole chip uses its full palette label");
        assert!(menu_items(&[], Some(&repeat)).is_empty(), "even Insert Selector needs a pick");

        let mixed = [pick("edge"), pick("face")];
        let view = Rect { pos: dvec2(20.0, 35.0), size: dvec2(1200.0, 800.0) };
        let strip: Vec<_> = commands(view, &mixed).into_iter().map(|(_, _, command)| command).collect();
        assert_eq!(strip, ["round", "chamfer", "pull", "hole", "shell"], "mixed picks keep the five strip commands in their original order");
        let items = menu_items(&mixed, Some(&repeat));
        let at = view.center();
        let layout = menu_layout(view, at, &items);
        let narrow_view = Rect { pos: view.pos, size: dvec2(220.0, 600.0) };
        let narrow = menu_layout(narrow_view, narrow_view.center(), &items);
        let right_press = dvec2(view.pos.x + view.size.x - 10.0, at.y);
        let right = menu_layout(view, right_press, &items);
        for (case, view, layout) in [("middle", view, &layout), ("narrow", narrow_view, &narrow), ("right edge", view, &right)] {
            assert_eq!(layout.pills.len(), items.len(), "{case}: every offered command has a pill");
            for item in &items {
                let index = layout.pills.iter().position(|p| p.0 == item.slot).expect("the layout preserves each offered slot");
                let (_, rect, lines) = &layout.pills[index];
                assert_eq!(lines.join(" "), item.label, "{case}: wrapping preserves the entire label in slot {}", item.slot);
                assert!(rect.is_inside_of(view.add_margin(dvec2(-8.0, -8.0))), "{case}: slot {} keeps the view margin", item.slot);
                assert!(layout.pills[..index].iter().all(|(_, other, _)| !rect.intersects(*other)), "{case}: slot {} is separate from every other pill", item.slot);
                let offset = rect.center() - layout.centre;
                let dx = (offset.x.abs() - rect.size.x * 0.5).max(0.0);
                let dy = (offset.y.abs() - rect.size.y * 0.5).max(0.0);
                assert!(dx.hypot(dy) > 14.0, "{case}: slot {} clears the circular hub", item.slot);
            }
        }
        assert_eq!(layout.centre, at, "a spacious view keeps the ring at the press");
        for (slot, rect, _) in &layout.pills {
            let end = rect.pos + rect.size;
            let aligned = match slot {
                0 => rect.center().x == at.x && end.y < at.y,
                1 => rect.pos.x > at.x && end.y < at.y,
                2 => rect.pos.x > at.x && rect.center().y == at.y,
                3 => rect.pos.x > at.x && rect.pos.y > at.y,
                4 => rect.center().x == at.x && rect.pos.y > at.y,
                5 => end.x < at.x && rect.pos.y > at.y,
                6 => end.x < at.x && rect.center().y == at.y,
                _ => false,
            };
            assert!(aligned, "slot {slot} places its pill in its assigned compass direction");
        }
        let narrow_round = narrow.pills.iter().find(|p| p.0 == 2).unwrap().1;
        assert!(narrow.pills.iter().any(|p| p.2.len() > 1) || narrow_round.pos.x - narrow.centre.x < 72.0, "in a narrow view labels wrap or the ring shrinks");
        assert!(right.centre.x < right_press.x, "a press near the right edge moves the smallest ring inward");
        // the hot command's words sit beside its pill, inside the view, over no pill and not the hub
        for (case, view, layout) in [("middle", view, &layout), ("narrow", narrow_view, &narrow), ("right edge", view, &right)] {
            let bounds = view.add_margin(dvec2(-8.0, -8.0));
            let hub = Rect { pos: layout.centre, size: DVec2::default() }.add_margin(DVec2::all(HUB_RADIUS));
            for item in &items {
                let (chip, lines) = chip_rect(view, layout, item.slot, layout.centre, &item.words).expect("every hot command has its words");
                assert_eq!(lines.join(" "), item.words, "{case}: the chip of slot {} keeps all its words", item.slot);
                assert!(chip.is_inside_of(bounds), "{case}: the chip of slot {} stays inside the view", item.slot);
                assert!(!chip.intersects(hub), "{case}: the chip of slot {} leaves the hub clear", item.slot);
                assert!(layout.pills.iter().all(|(_, pill, _)| !chip.intersects(*pill)), "{case}: the chip of slot {} covers no pill", item.slot);
            }
        }

        let selector = menu_items(&[pick("body")], None);
        let wrapped = menu_layout(view, dvec2(view.pos.x + 45.0, at.y), &selector);
        assert_eq!(wrapped.pills[0].2, ["Insert", "Selector"], "a body offers Insert Selector and its pill wraps at the space when both words fit separately");

        let mut menu = MarkingMenu::open(PaneId::PLATE_ZERO, view, at, items);
        assert_eq!(menu_hot(&menu, &layout, at + dvec2(10.0, 0.0)), None, "the dead radius leaves the ring idle");
        assert_eq!(menu_hot(&menu, &layout, at + dvec2(40.0, 0.0)), Some(2), "a 40 pt east stroke selects Round");
        assert_eq!(menu_hot(&menu, &layout, at + dvec2(-DIAGONAL, -DIAGONAL) * 40.0), None, "a 40 pt north-west stroke leaves the empty slot idle");
        let hole = layout.pills.iter().find(|p| p.0 == 6).unwrap().1;
        assert_eq!(menu_hot(&menu, &layout, hole.center()), Some(6), "pointing at the Hole pill selects Hole");
        menu.at = right_press;
        let chamfer = right.pills.iter().find(|p| p.0 == 3).unwrap().1;
        assert_eq!(menu_hot(&menu, &right, chamfer.center()), Some(3), "a shifted pill takes precedence over the sector from the press");
        assert_eq!(menu_hot(&menu, &right, right_press - dvec2(0.0, 20.0)), Some(0), "an inward shift keeps the stroke origin at the press");
    }
}

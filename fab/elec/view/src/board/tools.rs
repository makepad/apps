//! The Board pane's tool strip (E10 2, V9): one strip down the pane's left edge, groups parted at a
//! hairline. The modes are armed one gesture at a time as every canvas's are
//! (`crate::canvas::tools`). Under Select a drag on a part moves it, by its pads too (a small part
//! is mostly pads); a drag on a track moves its segment or carries on from its end.
//!
//! - Modes: Select (V); Route a track (X) draws copper from a pad or a track's end (a press on
//!   anything else starts nothing); Via places a via where it is clicked, on the net of the copper
//!   there (a drag under it is Select's); Zone, Outline, Keepout and Cutout drag rectangles or click polygon vertices; Measure drags a ruler
//!   between two points.
//! - Routing, acting at once: Route all, Route selected (the selection's unrouted nets), Clear routes
//!   (`plan::clear_routes`), and the route method (Straight, the grid router's tracks at 45 degree
//!   steps; Organic, the same tracks relaxed into smooth curves) as a flyout the host opens beside
//!   its button ([`BoardStripAction`]); and Shove (S), a switch: copper drawn by hand pushes the
//!   tracks in its way, or, off, walks round them. The route line keeps a job's progress and its
//!   Stop and Discard.
//! - Selection: Rotate (R or Shift+R), Flip (B), and Arrange (align courtyard edges or
//!   centres and distribute equal gaps). Shift-click adds or removes a part under Select.
//! - The board: Place (the placer, as a job: the selected parts again, or with none selected the
//!   parts not on the board yet, `plan::place_scope`), Tidy (move illegally placed parts in the
//!   selection, or on the whole board when no parts are selected), Fill (the ground net poured on the active copper layer, as F does with nothing
//!   under the pointer), Check rules (the board's Check list beside the strip), Update from the
//!   circuit (the circuit's parts the board does not place yet, placed beside the board), Export
//!   (the board's fabrication files, `elec.export`, which the host runs: [`BoardStripAction::Export`]).
//!
//! The host gives the strip its look: the pane's `tools` slot holds a widget with buttons by these
//! names (radio buttons for the modes, buttons for the actions). One that is missing is not
//! offered, and a host without a strip (the bench) keeps the keys.
//! - `strip`: the card holding them all; a pointer press, hover or scroll over it is the strip's,
//!   never the canvas's.
//! - `t_select`, `t_route`, `t_pair`, `t_tune`, `t_via`, `t_zone`, `t_outline`, `t_keepout`, `t_cutout`, `t_measure`: the modes.
//! - `a_route_all`, `a_route_selected`, `a_clear_routes`, `a_method`: routing; `a_shove`, an
//!   independent toggle (a radio button that turns off on a second click).
//! - `a_rotate`, `a_flip`, `a_arrange`: the selection.
//! - `a_place`, `a_tidy`, `a_fill`, `a_check`, `a_update`, `a_export`: the board.
//!
//! Cost: per pointer event one test against the strip's rectangle; per frame the strip's own
//! draw. Its buttons are written only when what they show changes ([`StripShow`]).

use super::plan::{Arrangement, Axis, Edge};
use fab_elec_core::{Angle, BoardId, CompId};
use fab_elec_route::Method;
use makepad_widgets_core::*;

/// The strip's parts a pointer over is the strip's.
pub const STRIP_PARTS: &[&[LiveId]] = &[ids!(strip)];

/// The boards with an outstanding Tidy press on the current plate, shared by their panes.
#[derive(Default)]
pub struct Tidying {
    pub boards: std::collections::HashSet<BoardId>,
}

/// A mode of the strip.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum BoardTool {
    /// The rest state: pick, marquee, and the plain drags.
    #[default]
    Select,
    /// Copper from a pad or a track's end.
    Route,
    /// Both legs of a declared differential pair (Shift+X).
    RoutePair,
    /// Refold a span of track toward its length match (T).
    Tune,
    /// A via where the click lands, on the net of the copper there.
    Via,
    /// A copper zone's rectangle, dragged on the active copper layer.
    Zone,
    Outline,
    Keepout,
    Cutout,
    /// A ruler between two points.
    Measure,
}

impl BoardTool {
    pub(super) fn draws_shape(self) -> bool {
        matches!(self, Self::Zone | Self::Outline | Self::Keepout | Self::Cutout)
    }
}

/// The Board pane's armed tool.
pub type Armed = crate::canvas::tools::Armed<BoardTool>;

const MODES: [(&[LiveId], BoardTool); 10] = [
    (ids!(t_select), BoardTool::Select),
    (ids!(t_route), BoardTool::Route),
    (ids!(t_pair), BoardTool::RoutePair),
    (ids!(t_tune), BoardTool::Tune),
    (ids!(t_via), BoardTool::Via),
    (ids!(t_zone), BoardTool::Zone),
    (ids!(t_outline), BoardTool::Outline),
    (ids!(t_keepout), BoardTool::Keepout),
    (ids!(t_cutout), BoardTool::Cutout),
    (ids!(t_measure), BoardTool::Measure),
];

/// What the strip's buttons show, written to them when it changes.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct StripShow {
    pub armed: Armed,
    /// The actions that can act now: Route all (unrouted nets and no job), Route selected (the
    /// selection holds unrouted nets and no job runs), Clear routes (copper to clear, no job).
    pub route_all: bool,
    pub route_selected: bool,
    pub clear_routes: bool,
    pub transform: bool,
    pub arrange: bool,
    pub tidy_busy: bool,
    /// Drawn copper pushes the tracks in its way.
    pub shove: bool,
}

/// What a click on the strip asks of the pane.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum StripPress {
    /// A mode's button (the pane tells a double click by its time, `canvas::tools::ToolClicks`).
    Tool(BoardTool),
    RouteAll,
    RouteSelected,
    ClearRoutes,
    /// The route method's button: its flyout.
    Method,
    Part(PartEdit),
    ArrangeMenu,
    /// The Shove switch: it flips.
    Shove,
    Place,
    Tidy,
    Fill,
    Check,
    Update,
    Export,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PartEdit { Rotate(Angle), Flip, Arrange(Arrangement) }

pub const ARRANGEMENTS: [(&str, Arrangement); 8] = [
    ("Align left", Arrangement::Align(Edge::Left)),
    ("Align right", Arrangement::Align(Edge::Right)),
    ("Align top", Arrangement::Align(Edge::Top)),
    ("Align bottom", Arrangement::Align(Edge::Bottom)),
    ("Align centre x", Arrangement::Align(Edge::CentreX)),
    ("Align centre y", Arrangement::Align(Edge::CentreY)),
    ("Distribute x", Arrangement::Distribute(Axis::X)),
    ("Distribute y", Arrangement::Distribute(Axis::Y)),
];

/// The route methods in the flyout's order.
pub const METHODS: [Method; 2] = [Method::Interactive, Method::Organic];

/// Between the strip's pane and its host, which draws the menus.
#[derive(Clone, Debug, PartialEq)]
pub enum BoardStripAction {
    /// Open the route method's flyout beside `anchor` (the method button's rectangle): each
    /// method's name and whether it is the one in use, in [`METHODS`] order.
    MethodMenu { rows: Vec<(&'static str, bool)>, anchor: Rect },
    /// The flyout's row picked, an index into [`METHODS`].
    Method(usize),
    ArrangeMenu { pane: WidgetUid, unit: BoardId, enabled: [bool; 8], anchor: Rect },
    Arrange { pane: WidgetUid, unit: BoardId, command: Arrangement },
    /// Tidy selected parts, or all unlocked parts on this board when the selection is empty.
    Tidy { unit: BoardId, parts: Vec<CompId> },
    /// Export the board's fabrication files (`elec.export` of this unit, run by the host).
    Export(BoardId),
}

/// The strip's presses among `actions`, in strip order. O(buttons).
pub fn strip_presses(cx: &mut Cx, tools: &WidgetRef, actions: &Actions) -> Vec<StripPress> {
    let mut presses: Vec<StripPress> = MODES.iter()
        .filter(|(id, _)| tools.radio_button(cx, id).clicked(actions))
        .map(|&(_, tool)| StripPress::Tool(tool))
        .collect();
    if tools.radio_button(cx, ids!(a_shove)).clicked(actions) {
        presses.push(StripPress::Shove);
    }
    for (id, press) in [
        (ids!(a_route_all), StripPress::RouteAll),
        (ids!(a_route_selected), StripPress::RouteSelected),
        (ids!(a_clear_routes), StripPress::ClearRoutes),
        (ids!(a_method), StripPress::Method),
        (ids!(a_rotate), StripPress::Part(PartEdit::Rotate(Angle::QUARTER))),
        (ids!(a_flip), StripPress::Part(PartEdit::Flip)),
        (ids!(a_arrange), StripPress::ArrangeMenu),
        (ids!(a_place), StripPress::Place),
        (ids!(a_tidy), StripPress::Tidy),
        (ids!(a_fill), StripPress::Fill),
        (ids!(a_check), StripPress::Check),
        (ids!(a_update), StripPress::Update),
        (ids!(a_export), StripPress::Export),
    ] {
        if tools.button(cx, id).clicked(actions) {
            presses.push(press);
        }
    }
    presses
}

/// The rectangle of the strip button opening a flyout.
pub fn flyout_anchor(cx: &Cx, tools: &WidgetRef, id: &[LiveId]) -> Option<Rect> {
    let button = tools.widget(cx, id);
    button.visible().then(|| button.area().rect(cx)).filter(|rect| rect.size.x > 0.0)
}

/// Write `show` to the strip's buttons. O(buttons); called only when `show` changes.
pub fn apply_strip(cx: &mut Cx, tools: &WidgetRef, show: &StripShow) {
    for (id, tool) in MODES {
        tools.radio_button(cx, id).set_active(cx, show.armed.tool == tool, Animate::No);
    }
    tools.radio_button(cx, ids!(a_shove)).set_active(cx, show.shove, Animate::No);
    tools.button(cx, ids!(a_tidy)).set_loading(cx, show.tidy_busy);
    for (id, on) in [
        (ids!(a_route_all), show.route_all),
        (ids!(a_route_selected), show.route_selected),
        (ids!(a_clear_routes), show.clear_routes),
        (ids!(a_rotate), show.transform),
        (ids!(a_flip), show.transform),
        (ids!(a_arrange), show.arrange),
    ] {
        tools.widget(cx, id).set_disabled(cx, !on);
    }
}

/// The method's argument to `elec.route`, and its name in the flyout.
pub fn method_name(method: Method) -> &'static str {
    match method {
        Method::Interactive => "Straight",
        Method::Organic => "Organic",
    }
}


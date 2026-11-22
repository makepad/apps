//! The Circuit pane's tool strip (E10 2): the sheet's verbs as a visible list along its left edge,
//! armed one gesture at a time as every canvas's are (`crate::canvas::tools`), and its status bar
//! along the bottom. Under Select the plain gestures stay what they were: a drag from a pin wires,
//! a drag on a part moves it, and Q marks the pin under the pointer no-connect.
//!
//! - Wire (W): a drag from a pin or a wire's end draws a wire; a press on anything else starts
//!   nothing. Done when the wire lands or is let go.
//! - Net label (L): a click on a wire or a connected pin opens its net's name for editing.
//! - Global label (Shift-L): a click on a pin opens a name; Enter ties the pin to the net of that
//!   name (made when there is none), drawn as name flags rather than wires.
//! - No-connect (Q): a click on a pin marks it, as Q does under the pointer with Select.
//! - Probe (B): a click on a pin or a wire puts a probe on its net.
//!
//! Below the modes, the strip's actions (buttons, not tools):
//! - Place part (A): the Library float opens with the cursor in its search.
//! - Power (P) opens the circuit's rails and the catalog's power flag beside the button. Pick a
//!   rail to tie a pin to it, or pick the flag to place it on the sheet.
//! - Undo and Redo at the strip's foot: the app's one history, as its caption's buttons; the host
//!   answers them, the view never sees them.
//!
//! The host gives the strip its look: the pane's `tools` slot holds a widget with radio buttons by
//! these names; one that is missing is not offered, and a host without a strip keeps the keys.
//! - `strip`: the card holding the tools, a pointer over which is the strip's.
//! - `t_select`, `t_wire`, `t_label`, `t_glabel`, `t_no_connect`, `t_probe`: the tools.
//! - `a_place`, `a_power`: the Place part and Power buttons.
//!
//! The Library (v9), the pane's `library` slot: a float over the sheet's top right (its top left
//! beside the strip once swapped), open down the sheet or folded to a tab. When the open panel
//! would take more than 40% of the sheet's width, the pane shows its tab. Opening the tab or
//! pressing Place part keeps it open while narrow, until folded by hand. Widening the sheet
//! restores the person's fold choice. The pane checks the width once per draw.
//! - `lib_open`: the open panel, its fixed width the float's; `lib_tab`: the folded tab, its
//!   fixed size the float's. A pointer over the one shown is the Library's.
//! - `lib_side`: swap the float to the sheet's other side; `lib_fold`: fold it to its tab;
//!   `lib_unfold`: the tab's button, open again.
//! - `library`: the `LibraryPanel`, whose `search` takes the keyboard on Place part.
//!
//! The status bar (V7), the pane's `status` slot, with labels and buttons by these names:
//! - `bar`: the row, a pointer over which is the bar's.
//! - `s_pos`: the pointer on the 1.27 mm grid, in mm; `s_hint`: what the tool or gesture does
//!   ([`hint`]); `s_counts`: parts, nets and open pins ([`counts`]).
//! - `z_out`, `z_in`, `z_fit`: zoom out, in, and fit the sheet.
//!
//! Cost: per pointer event one test against the strip's rectangle, the Library's and the bar's; per
//! frame their own draws. The strip's buttons are written only when the armed tool changes, the
//! bar's words only when they change; the counts are one pass over the pins when the circuit does.

use crate::canvas::tools::Armed;
use fab_elec_circuit::Circuit;
use makepad_widgets_core::*;

/// A tool of the strip.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub enum CircuitTool {
    /// The rest state: pick, reveal, and the plain drags.
    #[default]
    Select,
    Wire,
    Label,
    Global,
    NoConnect,
    Probe,
}

impl CircuitTool {
    /// The label tools, whose click opens the sheet's name field.
    pub fn names_nets(self) -> bool {
        matches!(self, Self::Label | Self::Global)
    }
}

/// The strip's parts a pointer over is the strip's.
pub const STRIP_PARTS: &[&[LiveId]] = &[ids!(strip)];

/// The status bar's parts a pointer over which is the bar's.
pub const BAR_PARTS: &[&[LiveId]] = &[ids!(bar)];
/// The Library float's open panel and its folded tab; only the one shown counts.
pub const LIBRARY_PARTS: &[&[LiveId]] = &[ids!(lib_open), ids!(lib_tab)];

/// One sheet grid step, in mm (the 1.27 mm connection grid, `fab_elec_core::sch`).
pub const STEP_MM: f64 = 1.27;

/// A zoom button's step: in by this factor, out by its inverse.
pub const ZOOM_STEP: f64 = 1.25;

/// A press on the status bar's zoom buttons.
#[derive(Copy, Clone, PartialEq, Debug)]
pub enum BarPress {
    Zoom(f64),
    Fit,
}

/// The status bar's zoom presses among `actions`.
pub fn bar_presses(cx: &mut Cx, status: &WidgetRef, actions: &Actions) -> Vec<BarPress> {
    let buttons: [(&[LiveId], BarPress); 3] =
        [(ids!(z_out), BarPress::Zoom(1.0 / ZOOM_STEP)), (ids!(z_in), BarPress::Zoom(ZOOM_STEP)), (ids!(z_fit), BarPress::Fit)];
    buttons.into_iter().filter(|&(id, _)| status.button(cx, id).clicked(actions)).map(|(_, press)| press).collect()
}

/// What is carried over the sheet, for the status bar's hint.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Carrying {
    Nothing,
    /// A drag from a pin or a wire's end.
    Wire,
    /// A Library part riding the pointer.
    Part,
    /// A rail from the Power flyout.
    Rail,
}

/// The status bar's hint: what a click or a drag does now, and the keys that go with it.
pub fn hint(tool: CircuitTool, carrying: Carrying) -> &'static str {
    match (carrying, tool) {
        (Carrying::Part, _) => "click to place · R turns · Shift-click places another · Esc puts it back",
        (Carrying::Rail, _) => "click a pin to tie it to the rail · Shift-click ties more · Esc",
        (Carrying::Wire, _) => "release on a pin, a wire or a tag · Esc",
        (Carrying::Nothing, CircuitTool::Select) => "G move or drag a part · a drag from a pin wires · A place · P power · W wire · B probe · F fit",
        (Carrying::Nothing, CircuitTool::Wire) => "drag from a pin to a pin, a wire or a tag · Esc",
        (Carrying::Nothing, CircuitTool::Label) => "click a wire or a pin to name its net",
        (Carrying::Nothing, CircuitTool::Global) => "click a pin to tie it to a net by name",
        (Carrying::Nothing, CircuitTool::NoConnect) => "click a pin to mark it not connected · Esc",
        (Carrying::Nothing, CircuitTool::Probe) => "click a pin or a wire to probe its net · Esc",
    }
}

/// "7 parts · 9 nets · 2 open pins": an open pin is on no net or alone on its net, and not marked
/// not connected. O(pins).
pub fn counts(c: &Circuit) -> String {
    let mut on_net = vec![0u32; c.nets.len()];
    for comp in &c.comps {
        for &(_, net) in &comp.conns {
            if let Some(n) = on_net.get_mut(net.ix()) { *n += 1; }
        }
    }
    let open: usize = c.comps.iter().map(|comp| (0..comp.part.pins.len()).filter(|&ix| {
        let pin = fab_elec_core::PinIx(ix as u16);
        !comp.nc.contains(&pin) && comp.conns.iter().find(|(p, _)| *p == pin)
            .is_none_or(|&(_, net)| on_net.get(net.ix()).is_some_and(|&n| n < 2))
    }).count()).sum();
    let plural = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    format!("{} · {} · {}", plural(c.comps.len(), "part", "parts"), plural(c.nets.len(), "net", "nets"), plural(open, "open pin", "open pins"))
}

const TOOLS: [(&[LiveId], CircuitTool); 6] = [
    (ids!(t_select), CircuitTool::Select),
    (ids!(t_wire), CircuitTool::Wire),
    (ids!(t_label), CircuitTool::Label),
    (ids!(t_glabel), CircuitTool::Global),
    (ids!(t_no_connect), CircuitTool::NoConnect),
    (ids!(t_probe), CircuitTool::Probe),
];

/// The tools whose buttons were clicked among `actions`, in strip order. O(tools).
pub fn strip_presses(cx: &mut Cx, tools: &WidgetRef, actions: &Actions) -> Vec<CircuitTool> {
    TOOLS.iter().filter(|(id, _)| tools.radio_button(cx, id).clicked(actions)).map(|&(_, tool)| tool).collect()
}

/// Between the strip's view and its host, which draws the menus.
#[derive(Clone, Debug, PartialEq)]
pub enum StripAction {
    /// Open the Power flyout beside the button with (net name, note) rows, ground first,
    /// followed by the power flag when the catalog offers one.
    PowerMenu { rails: Vec<(String, String)>, has_power_flag: bool, anchor: Rect },
    /// The rail picked in the flyout, by net name: it rides the pointer over the sheet.
    Rail(String),
    /// Carry the catalog's power flag for placement on the sheet.
    PowerFlag,
}

/// Whether the strip's Power button was clicked among `actions`.
pub fn power_pressed(cx: &mut Cx, tools: &WidgetRef, actions: &Actions) -> bool {
    tools.button(cx, ids!(a_power)).clicked(actions)
}

/// The Power button's rectangle, where its flyout opens; None while the strip has none.
pub fn power_anchor(cx: &Cx, tools: &WidgetRef) -> Option<Rect> {
    let button = tools.widget(cx, ids!(a_power));
    button.visible().then(|| button.area().rect(cx)).filter(|rect| rect.size.x > 0.0)
}

/// Whether the strip's Place part button was clicked among `actions`.
pub fn place_pressed(cx: &mut Cx, tools: &WidgetRef, actions: &Actions) -> bool {
    tools.button(cx, ids!(a_place)).clicked(actions)
}

/// Whether the strip's Annotate button was clicked among `actions`.
pub fn annotate_pressed(cx: &mut Cx, tools: &WidgetRef, actions: &Actions) -> bool {
    tools.button(cx, ids!(a_annotate)).clicked(actions)
}

/// Whether the strip's Check button was clicked among `actions`.
pub fn check_pressed(cx: &mut Cx, tools: &WidgetRef, actions: &Actions) -> bool {
    tools.button(cx, ids!(a_check)).clicked(actions)
}

/// Shows `armed` on the strip's buttons. O(tools); called only when it changes.
pub fn apply_strip(cx: &mut Cx, tools: &WidgetRef, armed: Armed<CircuitTool>) {
    for (id, tool) in TOOLS {
        tools.radio_button(cx, id).set_active(cx, armed.tool == tool, Animate::No);
    }
}

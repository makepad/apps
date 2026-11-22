//! The Strength editor: what would break first and which way up helps, read from the
//! current slice when a person asks, and the fixes they tick for it as one undo step. It calls the
//! same two tools the AI calls (`fab.strength_plan`, `fab.strengthen`) and shows what they answer, so
//! the panel and the chat write the same program; the app makes the calls and hands this module
//! their text. Turning the part, turning a limb and thickening change the design: nothing ticks them for
//! a person; solid infill keeps the shape, and a row whose own fix it is comes ticked.

use crate::api::*;
use crate::params::GhostBody;
use makepad_script::syntax::{parse_value, Node};
use makepad_widgets_core::widget_tree::CxWidgetExt;
use makepad_widgets_core::*;
use std::collections::HashMap;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabStrengthListBase = #(FabStrengthList::register_widget(vm))
    mod.widgets.FabStrengthList = set_type_default() do mod.widgets.FabStrengthListBase{
        width: Fill height: Fit flow: Down spacing: 0
        Row := View{
            width: Fill height: Fit flow: Right spacing: 4
            align: Align{y: 0.5}
            padding: Inset{left: 6 right: 4 top: 4 bottom: 4}
            cursor: MouseCursor.Hand
            show_bg: true
            draw_bg +: {
                selected: instance(0.0)
                pixel: fn() {
                    let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                    sdf.rect(0.0, 0.0, self.rect_size.x, self.rect_size.y)
                    sdf.fill(fab.color_editor.mix(fab.color_selection_bg, self.selected))
                    sdf.rect(0.0, 0.0, 2.0 * self.selected, self.rect_size.y)
                    sdf.fill(fab.color_accent)
                    return sdf.result
                }
            }
            tick_tip := FabTip{text: "Ticked: Apply fixes this place"
                tick := mod.widgets.FabCheckBox{width: 16 text: "" a11y: {label: "Fix this place"}}
            }
            text := View{
                width: Fill height: Fit flow: Down spacing: 1
                what := mod.widgets.FabLabel{width: Fill text: "" max_lines: 2}
                there := mod.widgets.FabLabelSmall{width: Fill text: "" max_lines: 2}
                cost := mod.widgets.FabLabelSmall{width: Fill text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis}
            }
            side := View{
                width: Fit height: Fit flow: Down spacing: 2
                fix_tip := FabTip{text: "What fixes it: solid infill fills what's sparse there, thicken raises its surface; a turn lays it flat"
                    fix := PropDropDown{width: 110 a11y: {label: "Fix"}}
                }
                test_tip := FabTip{text: "Add a test print round this place, to print just this piece"
                    test_print := mod.widgets.FabButton{
                        width: 110 height: theme.prop_row_height
                        text: "Test print" a11y: {label: "Test print this place"}
                    }
                }
            }
        }
    }

    // The Strength editor: what would break first, from the slice when a person asks, and its
    // fixes as one undo step. Find and its choices head it and Apply ends it, both in view
    // however long the list between them.
    mod.widgets.FabStrengthPanelBase = #(FabStrengthPanel::register_widget(vm))
    mod.widgets.FabStrengthPanel = set_type_default() do mod.widgets.FabStrengthPanelBase{
        width: Fill height: Fill flow: Down
        show_bg: true draw_bg +: { color: fab.color_editor }
        body := View{
            width: Fill height: Fill flow: Down
            padding: Inset{left: theme.prop_pad right: theme.prop_pad top: 6 bottom: 6}
            find_row := View{
                width: Fill height: Fit flow: Right spacing: 6
                align: Align{y: 0.5}
                padding: Inset{left: 10 right: 6 top: 2 bottom: 2}
                min_tip := FabTip{text: "How thin counts as weak. Left empty: 6% of the part's longest side"
                    min_row := View{
                        width: Fit height: Fit flow: Right spacing: 4
                        align: Align{y: 0.5}
                        mod.widgets.FabLabelSmall{text: "Minimum thickness"}
                        min := mod.widgets.FabInput{width: 44 empty_text: "auto" a11y: {label: "Minimum thickness in millimetres"}}
                        mod.widgets.FabLabelSmall{text: "mm"}
                    }
                }
                find_tip := FabTip{text: "What would break first and which way up helps, from the current slice"
                    find := mod.widgets.FabButton{
                        text: "Find" a11y: {label: "Find what would break first"}
                        height: theme.prop_row_height padding: Inset{left: 8 right: 8}
                    }
                }
            }
            goal_row := View{
                width: Fill height: Fit flow: Right spacing: 6
                align: Align{y: 0.5}
                padding: Inset{left: 10 right: 6 top: 2 bottom: 2}
                goal_tip := FabTip{text: "What the way up is chosen for: its limbs lying flat, or the least support"
                    goal_view := View{
                        width: Fit height: Fit flow: Right spacing: 4
                        align: Align{y: 0.5}
                        mod.widgets.FabLabelSmall{text: "Orient for"}
                        goal := PropDropDown{width: 96 labels: ["Strength" "Least support"] a11y: {label: "Orient for"}}
                    }
                }
                max_tip := FabTip{text: "The most a limb is offered a turn at its joint. Past it, it's left alone and says so"
                    max_view := View{
                        width: Fit height: Fit flow: Right spacing: 4
                        align: Align{y: 0.5}
                        mod.widgets.FabLabelSmall{text: "Max angle"}
                        max_angle := mod.widgets.FabInput{width: 36 empty_text: "15" a11y: {label: "Max angle in degrees"}}
                        mod.widgets.FabLabelSmall{text: "deg"}
                    }
                }
            }
            rows := mod.widgets.FabScroll{
                note := mod.widgets.FabLabelDim{width: Fill text: "" margin: Inset{left: 10 right: 6 top: 2 bottom: 2}}
                // what an Apply did, over the list it leaves: where the eye is once the list is read again
                after := mod.widgets.FabLabelDim{width: Fill text: "" margin: Inset{left: 10 right: 6 top: 2 bottom: 2}}
                list := mod.widgets.FabStrengthList{}
            }
            apply_row := View{
                width: Fill height: Fit flow: Right spacing: 6
                align: Align{y: 0.5}
                padding: Inset{left: 10 right: 6 top: 4 bottom: 2}
                apply_tip := FabTip{text: "Apply the ticked fixes as one undo step; the plate reslices and the list says what they did"
                    apply := mod.widgets.FabButton{
                        text: "Apply" a11y: {label: "Apply the ticked fixes"}
                        height: theme.prop_row_height padding: Inset{left: 8 right: 8}
                    }
                }
            }
        }
    }
}

/// One fix a row offers.
#[derive(Clone, Debug, PartialEq)]
pub struct StrengthFix {
    /// What the tool calls it.
    pub fix: String,
    pub label: String,
    /// Thicken: to how thick, mm.
    pub to: Option<f64>,
    /// What it costs, an estimate: `+2.1 g, +4 min`.
    pub cost: String,
    /// Orient: the way up and the turn about it a row of the table says, and the part.
    pub up: Option<[f64; 3]>,
    pub spin: Option<f64>,
}

/// Which list a row comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RowKind {
    #[default]
    Spot,
    Orient,
    Tube,
}

/// A tube's turn under a way up, or why there's none.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LimbTurn {
    pub fix: Option<StrengthFix>,
    pub why: String,
}

/// One weak spot, as its row shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct StrengthRow {
    pub id: String,
    /// `Neck: 6.2 mm`.
    pub what: String,
    /// What prints there now.
    pub there: String,
    pub fixes: Vec<StrengthFix>,
    /// The fix picked: a row's default from the slice to start.
    pub fix: usize,
    pub ticked: bool,
    /// The place's line on the plate and how far round it: what picking it draws.
    pub band: Option<([f64; 3], [f64; 3], f64)>,
    pub kind: RowKind,
    /// A tube's turn with the part turned as the Orient row says, and with it as it stands.
    pub turned: LimbTurn,
    pub stands: LimbTurn,
    /// A spot's box as `fab.test_print` takes it: part, name, centre and size.
    pub test_print: Option<(String, String, [f64; 3], [f64; 3])>,
}

/// What the section shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StrengthView {
    /// The field's text, mm: empty is what the find takes for a part of its size.
    pub min_thickness: String,
    /// What it's waiting for, if it is.
    pub busy: Option<&'static str>,
    /// The line over the list: how many, or why not.
    pub note: String,
    pub rows: Vec<StrengthRow>,
    pub picked: Option<String>,
    /// Before and after, from the slice after an Apply.
    pub after: Vec<String>,
    /// The plate's changed since the list was read.
    pub stale: bool,
    /// What the way up is chosen for: 0 the tubes, 1 the least support.
    pub goal: usize,
    /// The field's text, degrees: empty is the tool's own cap.
    pub max_angle: String,
    /// The part the Orient row turns.
    pub object: String,
}

/// A fix to send to `fab.strengthen`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pick {
    pub fix: String,
    pub spot: Option<String>,
    pub tube: Option<String>,
    pub to: Option<f64>,
    pub object: Option<String>,
    pub up: Option<[f64; 3]>,
    pub spin: Option<f64>,
    pub max_angle: Option<f64>,
}

/// What a person does in the editor. The app carries out what isn't only the editor's own.
#[derive(Clone, Debug, PartialEq)]
pub enum StrengthAction {
    MinThickness(String),
    Find,
    Apply,
    Tick { id: String, on: bool },
    PickFix { id: String, fix: usize },
    Pick(String),
    Goal(usize),
    MaxAngle(String),
    TestPrint(String),
}

pub const WAITING_FOR_SLICE: &str = "Waiting for the slice...";
pub const READING: &str = "Reading the slice...";
pub const APPLYING: &str = "Applying...";

/// How long something takes, as a person reads it.
fn duration(seconds: f64) -> String {
    let minutes = (seconds / 60.0).round() as i64;
    if minutes >= 60 {
        format!("{} h {:02}", minutes / 60, minutes % 60)
    } else if minutes > 0 {
        format!("{minutes} min")
    } else {
        format!("{} s", seconds.round() as i64)
    }
}

fn number(node: &Node, key: &str) -> Option<f64> {
    node.field(key).and_then(|f| f.value.number())
}

fn text<'a>(node: &'a Node, key: &str) -> Option<&'a str> {
    node.field(key).and_then(|f| f.value.string())
}

fn items<'a>(node: &'a Node, key: &str) -> &'a [Node] {
    node.field(key).and_then(|f| f.value.items()).unwrap_or(&[])
}

fn point(node: &Node, key: &str) -> Option<[f64; 3]> {
    let list = items(node, key);
    Some([list.first()?.number()?, list.get(1)?.number()?, list.get(2)?.number()?])
}

fn capitalised(words: &str) -> String {
    let mut chars = words.chars();
    chars.next().map_or_else(String::new, |c| c.to_uppercase().collect::<String>() + chars.as_str())
}

/// A fix's row from the tool's answer.
fn fix_of(node: &Node) -> Option<(StrengthFix, bool)> {
    let fix = text(node, "fix")?.to_string();
    let to = number(node, "to");
    let label = match (fix.as_str(), to) {
        ("solid_infill", _) => "Solid infill".to_string(),
        ("thicken", Some(to)) => format!("Thicken to {to:.1} mm"),
        (other, _) => other.replace('_', " "),
    };
    let grams = number(node, "adds_grams").map(|g| format!("+{g:.1} g"));
    let time = number(node, "adds_s").map(|t| format!("+{}", duration(t)));
    let cost = [grams, time].into_iter().flatten().collect::<Vec<_>>().join(", ");
    let default = node.field("default").and_then(|f| f.value.ident()) == Some("true");
    Some((StrengthFix { fix, label, to, cost, up: None, spin: None }, default))
}

fn row_of(node: &Node) -> Option<StrengthRow> {
    let id = text(node, "id")?.to_string();
    let thickness = number(node, "thickness")?;
    let what = format!("{}: {thickness:.1} mm", capitalised(text(node, "where")?));
    let there = node.field("there").map_or_else(String::new, |f| {
        let walls = text(&f.value, "walls").unwrap_or_default();
        let infill = text(&f.value, "infill").unwrap_or_default();
        format!("Now: {walls} walls, {infill} infill")
    });
    let mut fixes = Vec::new();
    let mut fix = 0;
    for (i, (one, default)) in items(node, "fixes").iter().filter_map(fix_of).enumerate() {
        if default {
            fix = i;
        }
        fixes.push(one);
    }
    let band = node.field("band").and_then(|f| Some((point(&f.value, "a")?, point(&f.value, "b")?, number(&f.value, "radius")?)));
    let test_print = node.field("test_print").and_then(|f| {
        let v = &f.value;
        Some((text(v, "object")?.to_string(), text(v, "name")?.to_string(), point(v, "at")?, point(v, "size")?))
    });
    let ticked = fixes.get(fix).is_some_and(|f| f.fix == "solid_infill");
    Some(StrengthRow { id, what, there, ticked, fix, fixes, band, kind: RowKind::Spot, turned: LimbTurn::default(), stands: LimbTurn::default(), test_print })
}

/// The row that turns the part: the table's choices past the plate as it stands.
fn orient_row(table: &Node) -> Option<StrengthRow> {
    let rows = items(table, "rows");
    let first = rows.get(1)?;
    let mut fixes = Vec::new();
    for row in &rows[1..] {
        let grams = number(row, "support_planned_grams").map_or_else(
            || number(row, "support_estimate_mm3").map_or_else(String::new, |mm3| format!("support {mm3:.0} mm3 (estimate)")),
            |g| format!("support {g:.1} g"),
        );
        let height = number(row, "height").map_or_else(String::new, |h| format!(", {h:.0} mm tall"));
        let fits = if node_bool(row, "fits_bed") == Some(false) { ", doesn't fit the bed" } else { "" };
        fixes.push(StrengthFix {
            fix: "orient".to_string(),
            label: capitalised(text(row, "is")?),
            to: None,
            cost: format!("{grams}{height}{fits}"),
            up: point(row, "up"),
            spin: number(row, "spin"),
        });
    }
    let lying: Vec<String> = items(first, "tubes").iter().filter_map(|t| {
        Some(format!("{} {:.0} -> {:.0} deg", text(t, "name")?, number(t, "before")?, number(t, "after")?))
    }).collect();
    let what = if lying.is_empty() { "Turn the part".to_string() } else { format!("Turn the part: {}", lying.join(", ")) };
    Some(StrengthRow {
        id: "orient".to_string(),
        what,
        there: text(table, "says").unwrap_or_default().to_string(),
        fixes,
        fix: 0,
        ticked: false,
        band: None,
        kind: RowKind::Orient,
        turned: LimbTurn::default(),
        stands: LimbTurn::default(),
        test_print: None,
    })
}

fn node_bool(node: &Node, key: &str) -> Option<bool> {
    node.field(key).and_then(|f| f.value.ident()).map(|word| word == "true")
}

/// A tube's turn from one of the table's lists.
fn limb_turn(list: &[Node], tube: &str) -> LimbTurn {
    let Some(row) = list.iter().find(|row| text(row, "tube") == Some(tube)) else { return LimbTurn::default() };
    if node_bool(row, "can") != Some(true) {
        return LimbTurn { fix: None, why: text(row, "why").unwrap_or_default().to_string() };
    }
    let degrees = number(row, "degrees").unwrap_or(0.0);
    let with = text(row, "joint_with").unwrap_or_default();
    let also = items(row, "also_turns").iter().filter_map(|n| n.string()).collect::<Vec<_>>().join(", ");
    let cost = if also.is_empty() { format!("At its joint with {with}. Changes the design") } else { format!("At its joint with {with}. {also} turns too. Changes the design") };
    LimbTurn { fix: Some(StrengthFix { fix: "align".to_string(), label: format!("Turn {degrees:.0} deg"), to: None, cost, up: None, spin: None }), why: String::new() }
}

/// A tube's row: how it lies and what lying flat would gain. Its fix follows the Orient row's tick.
fn tube_row(node: &Node, table: Option<&Node>) -> Option<StrengthRow> {
    let id = text(node, "tube")?.to_string();
    let angle = number(node, "angle_now")?;
    let gain = number(node, "flat_gain_estimate").or_else(|| number(node, "gain_estimate")).unwrap_or(1.0);
    let band = node.field("band").and_then(|f| Some((point(&f.value, "a")?, point(&f.value, "b")?, number(&f.value, "radius")?)));
    let turned = table.map_or_else(LimbTurn::default, |t| limb_turn(items(t, "align"), &id));
    let stands = table.map_or_else(LimbTurn::default, |t| limb_turn(items(t, "align_as_it_stands"), &id));
    Some(StrengthRow {
        what: format!("{}: {angle:.0} deg to the bed", capitalised(text(node, "name")?)),
        there: if gain > 1.05 { format!("Flat: about {gain:.1}x stronger (estimate)") } else { "Lies flat".to_string() },
        id,
        fixes: Vec::new(),
        fix: 0,
        ticked: false,
        band,
        kind: RowKind::Tube,
        turned,
        stands,
        test_print: None,
    })
}

impl StrengthView {
    /// A find's answer.
    pub fn found(&mut self, text_of_answer: &str) {
        self.busy = None;
        let Ok(answer) = parse_value(text_of_answer) else {
            self.note = "Couldn't read the answer".to_string();
            return;
        };
        self.stale = false;
        let spots: Vec<StrengthRow> = items(&answer, "spots").iter().filter_map(row_of).collect();
        let under = number(&answer, "min_thickness").unwrap_or(0.0);
        let table = answer.field("orientation").map(|f| &f.value);
        self.object = table.and_then(|t| text(t, "object")).unwrap_or_default().to_string();
        let count = spots.len();
        self.rows = table.and_then(orient_row).into_iter().collect();
        self.rows.extend(items(&answer, "tubes").iter().filter_map(|tube| tube_row(tube, table)));
        self.rows.extend(spots);
        self.follow_orient();
        self.note = match count {
            0 => format!("Nothing under {under:.1} mm."),
            1 => format!("One place under {under:.1} mm."),
            count => format!("{count} places under {under:.1} mm, what would break first on top."),
        };
        self.after = answer.field("fixed").map_or_else(Vec::new, |f| {
            let mut lines: Vec<String> = items(&f.value, "spots").iter().filter_map(|spot| {
                let before = number(spot, "thickness_before")?;
                let was = capitalised(text(spot, "where")?);
                Some(match number(spot, "thickness_after") {
                    Some(after) => format!("{was}: {before:.1} -> {after:.1} mm"),
                    None => format!("{was}: {before:.1} mm, {}", text(spot, "after")?),
                })
            }).collect();
            if let (Some(a), Some(b)) = (number(&f.value, "plate_grams_before"), number(&f.value, "plate_grams_after")) {
                lines.push(format!("Plate: {a:.1} -> {b:.1} g"));
            }
            if let (Some(a), Some(b)) = (number(&f.value, "plate_print_s_before"), number(&f.value, "plate_print_s_after")) {
                lines.push(format!("Extruding: {} -> {}", duration(a), duration(b)));
            }
            lines
        });
        self.picked = None;
    }

    /// What's ticked, for `fab.strengthen`.
    pub fn ticked(&self) -> Vec<Pick> {
        let max_angle = self.max_angle.trim().parse::<f64>().ok().filter(|a| *a > 0.0);
        self.rows.iter().filter(|r| r.ticked).filter_map(|r| {
            let fix = r.fixes.get(r.fix)?;
            let mut pick = Pick { fix: fix.fix.clone(), spot: None, tube: None, to: fix.to, object: None, up: fix.up, spin: fix.spin, max_angle: None };
            match r.kind {
                RowKind::Spot => pick.spot = Some(r.id.clone()),
                RowKind::Tube => {
                    pick.tube = Some(r.id.clone());
                    pick.max_angle = max_angle;
                }
                RowKind::Orient => pick.object = (!self.object.is_empty()).then(|| self.object.clone()),
            }
            Some(pick)
        }).collect()
    }

    /// A limb lies flat under the way up the part's going to have: what a tube's row offers follows
    /// the Orient row's tick, and a turn it no longer offers can't stay ticked.
    fn follow_orient(&mut self) {
        let turning = self.rows.iter().any(|r| r.kind == RowKind::Orient && r.ticked);
        for row in self.rows.iter_mut().filter(|r| r.kind == RowKind::Tube) {
            let turn = if turning { &row.turned } else { &row.stands };
            row.fixes = turn.fix.clone().into_iter().collect();
            row.fix = 0;
            if row.fixes.is_empty() {
                row.ticked = false;
            }
        }
    }

    /// Why a tube's row offers no turn, under the way up it goes by.
    pub fn no_turn(&self, row: &StrengthRow) -> String {
        let turning = self.rows.iter().any(|r| r.kind == RowKind::Orient && r.ticked);
        if turning { row.turned.why.clone() } else { row.stands.why.clone() }
    }

    /// The picked place's body, to draw: a rod round its line.
    pub fn highlight(&self) -> Option<GhostBody> {
        let row = self.rows.iter().find(|r| self.picked.as_deref() == Some(r.id.as_str()))?;
        let (a, b, radius) = row.band?;
        Some(rod_body(a, b, radius))
    }
}

/// Carries out what's the section's own; the app does [`StrengthAction::Find`] and
/// [`StrengthAction::Apply`], which call the tools.
pub fn apply(state: &mut AppState, action: &StrengthAction) {
    let view = &mut state.ui.strength;
    match action {
        StrengthAction::MinThickness(text) => view.min_thickness = text.clone(),
        StrengthAction::Tick { id, on } => {
            if let Some(row) = view.rows.iter_mut().find(|r| r.id == *id) {
                row.ticked = *on;
            }
            view.follow_orient();
        }
        StrengthAction::PickFix { id, fix } => {
            if let Some(row) = view.rows.iter_mut().find(|r| r.id == *id) {
                row.fix = *fix;
            }
        }
        StrengthAction::Pick(id) => view.picked = if view.picked.as_deref() == Some(id.as_str()) { None } else { Some(id.clone()) },
        StrengthAction::Goal(goal) => view.goal = *goal,
        StrengthAction::MaxAngle(text) => view.max_angle = text.clone(),
        StrengthAction::Find | StrengthAction::Apply | StrengthAction::TestPrint(_) => {}
    }
}

const ROD_SIDES: u32 = 16;

/// A closed rod with flat ends from `a` to `b`, or a short one upright round a single point.
fn rod_body(a: [f64; 3], b: [f64; 3], radius: f64) -> GhostBody {
    let d: [f64; 3] = std::array::from_fn(|k| b[k] - a[k]);
    let length = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt();
    let (a, axis) = if length > 1e-6 { (a, d.map(|x| x / length)) } else { ([a[0], a[1], a[2] - radius], [0.0, 0.0, 1.0]) };
    let long = if length > 1e-6 { length } else { 2.0 * radius };
    let seed = if axis[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [0.0, 1.0, 0.0] };
    let cross = |p: [f64; 3], q: [f64; 3]| [p[1] * q[2] - p[2] * q[1], p[2] * q[0] - p[0] * q[2], p[0] * q[1] - p[1] * q[0]];
    let x = cross(axis, seed);
    let l = (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt();
    let x = x.map(|v| v / l);
    let y = cross(axis, x);
    let mut body = GhostBody::default();
    // two rings, then the end caps' middles
    for t in [0.0, long] {
        for i in 0..ROD_SIDES {
            let (sin, cos) = (i as f64 * std::f64::consts::TAU / ROD_SIDES as f64).sin_cos();
            body.positions.push(std::array::from_fn(|k| a[k] + axis[k] * t + radius * (x[k] * cos + y[k] * sin)).into());
        }
    }
    let (first, second) = (2 * ROD_SIDES, 2 * ROD_SIDES + 1);
    body.positions.push(a.into());
    body.positions.push(std::array::from_fn::<f64, 3, _>(|k| a[k] + axis[k] * long).into());
    for i in 0..ROD_SIDES {
        let next = (i + 1) % ROD_SIDES;
        let (p, q, r, t) = (i, next, i + ROD_SIDES, next + ROD_SIDES);
        body.triangles.extend([[p, r, q], [q, r, t], [first, p, q], [second, t, r]]);
        body.edges.extend([[p, q], [r, t]]);
    }
    for i in [0, ROD_SIDES / 2] {
        body.edges.push([i, i + ROD_SIDES]);
    }
    body
}

#[derive(Script, Widget)]
pub struct FabStrengthList {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[rust]
    templates: HashMap<LiveId, ScriptObjectRef>,
    #[rust]
    items: HashMap<LiveId, WidgetRef>,
    /// The rows of the last draw, each with its widget's id, whether it's picked and why it has no fix.
    #[rust]
    shown: Vec<(StrengthRow, LiveId, bool, String)>,
    /// What each row was last given.
    #[rust]
    applied: HashMap<LiveId, (StrengthRow, bool, String)>,
    #[redraw]
    #[rust]
    area: Area,
}

impl ScriptHook for FabStrengthList {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                            self.templates.insert(id, vm.new_object_ref(t));
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            for w in self.items.values_mut() {
                if let Some(t) = self.templates.get(&live_id!(Row)) {
                    let v: ScriptValue = t.as_object().into();
                    w.script_apply(vm, apply, scope, v);
                }
            }
            self.applied.clear();
        }
    }
}

impl FabStrengthList {
    fn item(&mut self, cx: &mut Cx, id: LiveId) -> Option<WidgetRef> {
        if let Some(w) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&live_id!(Row))?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        self.items.insert(id, w.clone());
        Some(w)
    }
}

impl Widget for FabStrengthList {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Weak spots");
        node.set_size = self.shown.len() as u32;
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        for (row, id, _, _) in &self.shown {
            let Some(w) = self.items.get(id) else { continue };
            w.handle_event(cx, event, scope);
            if let Event::Actions(actions) = event {
                if let Some(on) = w.check_box(cx, ids!(tick)).changed(actions) {
                    cx.action(StrengthAction::Tick { id: row.id.clone(), on });
                }
                if let Some(fix) = w.drop_down(cx, ids!(fix)).changed(actions) {
                    cx.action(StrengthAction::PickFix { id: row.id.clone(), fix });
                }
                if w.button(cx, ids!(test_print)).clicked(actions) {
                    cx.action(StrengthAction::TestPrint(row.id.clone()));
                }
            }
            match event.hits(cx, w.area()) {
                Hit::FingerHoverIn(_) => cx.set_cursor(MouseCursor::Hand),
                Hit::FingerUp(fe) if fe.is_over && !fe.cancelled && fe.device.is_primary_hit() && fe.was_tap() => cx.action(StrengthAction::Pick(row.id.clone())),
                _ => {}
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let view = &state.ui.strength;
        self.shown = view.rows.iter().map(|row| (row.clone(), LiveId::from_str(&row.id), view.picked.as_deref() == Some(row.id.as_str()), view.no_turn(row))).collect();
        cx.begin_turtle(walk, self.layout);
        for k in 0..self.shown.len() {
            let (row, id, picked, no_turn) = self.shown[k].clone();
            let Some(w) = self.item(cx, id) else { continue };
            let look = (row.clone(), picked, no_turn.clone());
            if self.applied.get(&id) != Some(&look) {
                w.label(cx, ids!(what)).set_text(cx, &row.what);
                w.label(cx, ids!(there)).set_text(cx, &row.there);
                let fix = row.fixes.get(row.fix);
                let none = match row.kind {
                    RowKind::Spot => "Nothing offered: thin, with nothing here to fill",
                    _ => no_turn.as_str(),
                };
                w.label(cx, ids!(cost)).set_text(cx, fix.map_or(none, |f| f.cost.as_str()));
                w.check_box(cx, ids!(tick)).set_active(cx, row.ticked, Animate::No);
                w.widget(cx, ids!(tick_tip)).set_visible(cx, !row.fixes.is_empty());
                w.widget(cx, ids!(fix_tip)).set_visible(cx, !row.fixes.is_empty());
                w.widget(cx, ids!(test_tip)).set_visible(cx, row.test_print.is_some());
                let picker = w.drop_down(cx, ids!(fix));
                picker.set_labels(cx, row.fixes.iter().map(|f| f.label.clone()).collect());
                picker.set_selected_item(cx, row.fix);
                let mut face = w.clone();
                let selected = if picked { 1.0 } else { 0.0 };
                script_apply_eval!(cx, face, {draw_bg +: {selected: #(selected)}});
                self.applied.insert(id, look);
            }
            w.draw_all(cx, &mut Scope::empty());
        }
        cx.end_turtle_with_area(&mut self.area);
        DrawStep::done()
    }
}

/// The Strength editor: its rows' state from [`AppState`]'s `ui.strength` on each draw, O(1)
/// apart from the list's own rows; each control an action for the app.
#[derive(Script, ScriptHook, Widget)]
pub struct FabStrengthPanel {
    #[deref]
    view: View,
}

impl Widget for FabStrengthPanel {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "Strength");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if scope.data.get::<AppState>().is_some_and(|s| !s.ui.on_screen(crate::ui::editors::Editor::Strength)) {
            // off screen: none of its controls can be pressed
            return;
        }
        let v = &self.view;
        if let Some(text) = v.text_input(cx, ids!(body.min)).changed(actions) {
            cx.action(StrengthAction::MinThickness(text));
        }
        if let Some(text) = v.text_input(cx, ids!(body.max_angle)).changed(actions) {
            cx.action(StrengthAction::MaxAngle(text));
        }
        if let Some(goal) = v.drop_down(cx, ids!(body.goal)).changed(actions) {
            cx.action(StrengthAction::Goal(goal));
        }
        if v.button(cx, ids!(body.find)).clicked(actions) {
            cx.action(StrengthAction::Find);
        }
        if v.button(cx, ids!(body.apply)).clicked(actions) {
            cx.action(StrengthAction::Apply);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            let v = &self.view;
            let strength = &state.ui.strength;
            v.widget(cx, ids!(body.find)).set_disabled(cx, state.plate.parts.is_empty() || strength.busy.is_some());
            let said = match strength.busy {
                Some(busy) => busy.to_string(),
                None if strength.stale => format!("{} The plate has changed: Find again.", strength.note),
                None => strength.note.clone(),
            };
            v.widget(cx, ids!(body.note)).set_visible(cx, !said.is_empty());
            v.label(cx, ids!(body.note)).set_text(cx, &said);
            let ticked = strength.ticked().len();
            v.widget(cx, ids!(body.apply_row)).set_visible(cx, strength.rows.iter().any(|r| !r.fixes.is_empty()));
            v.widget(cx, ids!(body.apply)).set_disabled(cx, ticked == 0 || strength.busy.is_some() || strength.stale);
            v.button(cx, ids!(body.apply)).set_text(cx, &match ticked {
                0 => "Apply".to_string(),
                1 => "Apply 1 fix".to_string(),
                count => format!("Apply {count} fixes"),
            });
            v.widget(cx, ids!(body.after)).set_visible(cx, !strength.after.is_empty());
            v.label(cx, ids!(body.after)).set_text(cx, &strength.after.join("\n"));
            let typed = v.text_input(cx, ids!(body.min));
            if typed.text() != strength.min_thickness {
                typed.set_text(cx, &strength.min_thickness);
            }
            let typed = v.text_input(cx, ids!(body.max_angle));
            if typed.text() != strength.max_angle {
                typed.set_text(cx, &strength.max_angle);
            }
            v.drop_down(cx, ids!(body.goal)).set_selected_item(cx, strength.goal);
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"{current: true min_thickness: 6.5 spots: [
        {id: "spot1" where: "neck" thickness: 5.2 there: {walls: "3" infill: "15%"} band: {a: [1 2 3] b: [1 2 13] radius: 4}
         fixes: [{fix: "solid_infill" default: false adds_grams: 1.25 adds_s: 300} {fix: "thicken" default: true to: 6.5 adds_grams: 2.5 adds_s: 90}]}
        {id: "spot2" where: "limb near [0 0 0]" thickness: 4.0 there: {walls: "3" infill: "15%"} fixes: []}
        {id: "spot3" where: "arm" thickness: 4.5 there: {walls: "3" infill: "15%"} fixes: [{fix: "solid_infill" default: true adds_grams: 0.5 adds_s: 60}]}
    ]}"#;

    #[test]
    fn a_find_reads_its_rows_and_ticks_only_what_keeps_the_shape() {
        let mut view = StrengthView::default();
        view.found(ANSWER);
        assert_eq!(view.rows.len(), 3);
        assert_eq!(view.rows[0].what, "Neck: 5.2 mm");
        assert_eq!(view.rows[0].there, "Now: 3 walls, 15% infill");
        assert_eq!(view.rows[0].fixes[view.rows[0].fix].label, "Thicken to 6.5 mm");
        assert_eq!(view.rows[0].fixes[0].cost, "+1.2 g, +5 min");
        assert!(!view.rows[0].ticked, "a thicken changes the shape: a person ticks it");
        assert!(!view.rows[1].ticked, "a row with nothing to offer can't be ticked");
        assert!(view.rows[2].ticked, "solid infill keeps the shape");
        let mut state = AppState::default();
        state.ui.strength = view;
        apply(&mut state, &StrengthAction::Tick { id: "spot1".to_string(), on: true });
        let picks = state.ui.strength.ticked();
        assert_eq!(picks.len(), 2);
        assert_eq!((picks[0].spot.as_deref(), picks[0].fix.as_str(), picks[0].to), (Some("spot1"), "thicken", Some(6.5)));
        assert_eq!((picks[1].spot.as_deref(), picks[1].fix.as_str()), (Some("spot3"), "solid_infill"));
        let view = &state.ui.strength;
        assert!(view.note.starts_with("3 places under 6.5 mm"), "{}", view.note);
    }

    const PLAN: &str = r#"{current: true min_thickness: 6.5 spots: [] says: "neck 2 deg"
        tubes: [{tube: "tube1" name: "neck" angle_now: 88 angle_after: 2 gain_estimate: 3.1}
                {tube: "tube2" name: "arm" angle_now: 10 angle_after: 58 gain_estimate: 0.8}]
        orientation: {object: "man/man" says: "neck 2 deg; arm stays" rows: [
            {is: "as it stands" up: [0 0 1] spin: 0 height: 100 fits_bed: true support_estimate_mm3: 10 tubes: []}
            {is: "tubes flattest" up: [0 1 0] spin: 30 height: 30 fits_bed: true support_estimate_mm3: 900
             tubes: [{name: "neck" before: 88 after: 2}]}]
         align: [{tube: "tube2" can: false degrees: 58 why: "58 deg off flat, past the 15 deg cap"}]
         align_as_it_stands: [{tube: "tube2" can: true degrees: 10 joint_with: "torso"}]}}"#;

    #[test]
    fn a_plan_offers_a_turn_of_the_part_and_of_a_limb_and_ticks_neither() {
        let mut state = AppState::default();
        state.ui.strength.found(PLAN);
        let view = &state.ui.strength;
        assert_eq!(view.rows.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["orient", "tube1", "tube2"]);
        assert_eq!(view.rows[0].what, "Turn the part: neck 88 -> 2 deg");
        assert_eq!(view.rows[0].fixes[0].label, "Tubes flattest");
        assert_eq!(view.rows[1].what, "Neck: 88 deg to the bed");
        assert!(view.ticked().is_empty(), "these change the design: a person ticks them");
        // as it stands the arm's a small turn; with the part turned it's past the cap
        assert_eq!(view.rows[2].fixes[0].label, "Turn 10 deg");
        apply(&mut state, &StrengthAction::Tick { id: "tube2".into(), on: true });
        assert_eq!(state.ui.strength.ticked()[0].tube.as_deref(), Some("tube2"));
        apply(&mut state, &StrengthAction::Tick { id: "orient".into(), on: true });
        let view = &state.ui.strength;
        assert!(view.rows[2].fixes.is_empty(), "no turn past the cap");
        assert!(view.no_turn(&view.rows[2]).contains("past the 15 deg cap"));
        let picks = view.ticked();
        assert_eq!(picks.len(), 1, "and a turn it no longer offers isn't ticked: {picks:?}");
        assert_eq!((picks[0].fix.as_str(), picks[0].object.as_deref(), picks[0].up, picks[0].spin), ("orient", Some("man/man"), Some([0.0, 1.0, 0.0]), Some(30.0)));
    }

    #[test]
    fn nothing_thin_says_so() {
        let mut view = StrengthView::default();
        view.found(r#"{current: true min_thickness: 3 spots: []}"#);
        assert_eq!(view.note, "Nothing under 3.0 mm.");
    }

    #[test]
    fn the_slice_after_says_before_and_after() {
        let mut view = StrengthView::default();
        view.found(r#"{current: true min_thickness: 3 spots: [] fixed: {spots: [{where: "neck" thickness_before: 5.2 after: "no longer under the minimum"} {where: "leg" thickness_before: 6 after: "solid infill placed"} {where: "arm" thickness_before: 4 thickness_after: 4.5 after: "still under the minimum"}] plate_grams_before: 40 plate_grams_after: 42 plate_print_s_before: 3600 plate_print_s_after: 3900}}"#);
        assert_eq!(view.after, ["Neck: 5.2 mm, no longer under the minimum", "Leg: 6.0 mm, solid infill placed", "Arm: 4.0 -> 4.5 mm", "Plate: 40.0 -> 42.0 g", "Extruding: 1 h 00 -> 1 h 05"]);
    }

    #[test]
    fn picking_a_row_draws_a_rod_round_its_place_and_picking_again_stops() {
        let mut state = AppState::default();
        state.ui.strength.found(ANSWER);
        apply(&mut state, &StrengthAction::Pick("spot1".into()));
        let body = state.ui.strength.highlight().expect("a body");
        assert_eq!(body.positions.len() as u32, 2 * ROD_SIDES + 2);
        assert!(!body.triangles.is_empty() && !body.edges.is_empty());
        apply(&mut state, &StrengthAction::Pick("spot1".into()));
        assert!(state.ui.strength.highlight().is_none());
    }
}

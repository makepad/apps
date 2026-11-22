//! `fab.workspace` describes the caption and area tree. `fab.workspace_edit` tries Splash
//! statements in order on the caller's probe. Each edit is the UI's own action. Only a
//! successful batch returns actions to dispatch, so the edit to the app is all or nothing.
//!
//! On a tool call, `run` costs O(statements × tree size). The caller's probe clone copies the
//! current workspace trees and one reference per saved layout snapshot.
//! Workspace lookup also visits the row and the trees inspected by the caption's visibility
//! rule. `describe` costs O(workspaces + areas + editors), counting the areas and tabs visited
//! for visibility and the supplied area rectangles. Neither tool runs per frame.

use crate::api::{Family, PlateMode, ShellAction};
use crate::ui::areas::{AreaId, AreaTree, DropAt, Instance, Moving, OpError, Side, WorkspaceOp};
use crate::ui::editors::{Editor, EDITORS, MENU};
use crate::workspaces::{ListOp, Origin, ShownBy, WorkspaceDef, WorkspaceId, Workspaces};
use makepad_model_script::lang::names::Names;
use makepad_script::syntax::{self, write::quote, Kind, Node};
use makepad_widgets_core::{Rect, SplitterAlign, SplitterAxis};
use std::collections::HashMap;
use std::fmt::Write;

/// One change the tool makes, as the UI makes it.
#[derive(Clone, Debug)]
pub enum WsEdit {
    Areas(WorkspaceOp),
    List(ListOp),
    Switch(WorkspaceId),
    Mode(PlateMode),
    /// `instance` in front of `area`, pointed at `thing` (`focus`): as a person brings the tab
    /// forward and clicks the thing. The caller resolves `thing` against the loaded design
    /// before it dispatches anything ([`WsEdit::points`]), and selects it.
    Focus { area: AreaId, instance: Instance, thing: Thing, board: Option<String> },
}

/// What `focus` points an editor at, by the words the electronics reads write: a net's name
/// (GND), a part's reference or path (R1, mcu.boot_r), a pin (R1.2, U1.PB0); `Any` is whichever
/// of those the text names (a part wins over a net of the same name, a net over a pin).
#[derive(Clone, Debug, PartialEq)]
pub enum Thing {
    Any(String),
    Net(String),
    Part(String),
    Pin(String),
}

impl Thing {
    /// The argument's name and its text.
    pub fn field(&self) -> (&'static str, &str) {
        match self {
            Self::Any(text) => ("focus", text),
            Self::Net(text) => ("net", text),
            Self::Part(text) => ("part", text),
            Self::Pin(text) => ("pin", text),
        }
    }
}

/// A thing `fab.show` shows, its kind as found in the design (a name alone is resolved first).
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub thing: Thing,
    pub board: Option<String>,
}

impl Shown {
    /// "net VCC", "part U1", "pin U1.PB0" (a still unresolved name as itself).
    pub fn words(&self) -> String {
        match &self.thing {
            Thing::Any(text) => text.clone(),
            Thing::Net(text) => format!("net {text}"),
            Thing::Part(text) => format!("part {text}"),
            Thing::Pin(text) => format!("pin {text}"),
        }
    }
}

/// The `fab.workspace_edit` statements `fab.show` stands for, and the editors they show the things
/// in. When the shown workspace's front editors can show every thing, each is focused in each of
/// them that can, and nothing moves. Otherwise a temporary layout titled `note` opens, copied from
/// the offered workspace whose front editors are the most of the Schematic and the Board (the
/// shown one first on a tie), the missing ones shown in it, and each thing focused in both.
/// O(workspaces × areas + things × areas).
pub fn show_statements(ws: &Workspaces, offered: &dyn Fn(&WorkspaceDef) -> bool, things: &[Shown], note: &str) -> (String, Vec<Editor>) {
    let arg = |shown: &Shown| {
        let (field, text) = shown.thing.field();
        let what = if field == "focus" { quote(text) } else { format!("{field}: {}", quote(text)) };
        match &shown.board {
            Some(board) => format!("{what}, board: {}", quote(board)),
            None => what,
        }
    };
    let fronts = |d: &WorkspaceDef| d.tree.areas();
    let shown_now = fronts(ws.current());
    if things.iter().all(|t| shown_now.iter().any(|(_, i)| shows_thing(i.editor, &t.thing))) {
        let mut text = String::new();
        let mut editors = Vec::new();
        for t in things {
            for (area, instance) in shown_now.iter().filter(|(_, i)| shows_thing(i.editor, &t.thing)) {
                let _ = write!(text, "focus({}, {})\n", area.0, arg(t));
                if !editors.contains(&instance.editor) {
                    editors.push(instance.editor);
                }
            }
        }
        return (text, editors);
    }
    let wanted = [Editor::Circuit, Editor::Board];
    let score = |d: &WorkspaceDef| wanted.iter().filter(|e| fronts(d).iter().any(|(_, i)| i.editor == **e)).count();
    let row: Vec<&WorkspaceDef> = ws.list.iter().filter(|d| offered(d)).collect();
    let mut from = (if ws.temp_shown() { "temp".to_string() } else { row.iter().position(|d| d.id == ws.current).map_or("temp".into(), |n| (n + 1).to_string()) }, score(ws.current()), ws.current());
    for (n, d) in row.iter().enumerate() {
        if score(d) > from.1 {
            from = ((n + 1).to_string(), score(d), *d);
        }
    }
    let mut text = format!("temporary({}, from: {})\n", quote(note), from.0);
    for editor in wanted.iter().filter(|e| !fronts(from.2).iter().any(|(_, i)| i.editor == **e)) {
        let _ = write!(text, "show({})\n", quote(editor.title()));
    }
    for t in things {
        for editor in wanted {
            let _ = write!(text, "focus({}, {})\n", quote(editor.title()), arg(t));
        }
    }
    (text, wanted.to_vec())
}

/// Which things an editor shows when pointed at them: the shared selection's views (the
/// Schematic and the Board every kind, the 3D view parts) and a Scope's lanes (nets).
fn shows_thing(editor: Editor, thing: &Thing) -> bool {
    match editor {
        Editor::Circuit | Editor::Board => true,
        Editor::Device => matches!(thing, Thing::Any(_) | Thing::Part(_)),
        Editor::Scope => matches!(thing, Thing::Any(_) | Thing::Net(_)),
        _ => false,
    }
}

impl WsEdit {
    /// The action the UI sends for it.
    pub fn action(&self) -> ShellAction {
        match self {
            Self::Areas(op) => ShellAction::EditAreas(*op),
            Self::List(op) => ShellAction::EditWorkspaces(op.clone()),
            Self::Switch(id) => ShellAction::SwitchWorkspace(*id),
            Self::Mode(mode) => ShellAction::SetPlateMode(*mode),
            // in front first; the pointing is the caller's ([`WsEdit::points`])
            Self::Focus { area, instance, .. } => ShellAction::EditAreas(WorkspaceOp::Select { area: *area, instance: *instance }),
        }
    }

    /// The `focus` statements' editors and things, in order: the caller resolves each against
    /// the design and points the editor at it.
    /// The board named with it (`board:`) narrows the search where several have the thing.
    pub fn points(edits: &[WsEdit]) -> impl Iterator<Item = (Instance, &Thing, Option<&str>)> {
        edits.iter().filter_map(|edit| match edit {
            Self::Focus { instance, thing, board, .. } => Some((*instance, thing, board.as_deref())),
            _ => None,
        })
    }

    /// The editors the edits pointed at, in order, each once: what the AI outline lights after
    /// the change (an area set, split off, brought forward, moved or focused).
    pub fn pointed(edits: &[WsEdit], probe: &Workspaces) -> Vec<Editor> {
        let tree = &probe.current().tree;
        let front = |area: AreaId| tree.stack_of(area).and_then(|(tabs, front)| tabs.get(front).map(|i| i.editor));
        let mut editors: Vec<Editor> = Vec::new();
        for edit in edits {
            let editor = match edit {
                Self::Areas(WorkspaceOp::SetEditor { editor, .. } | WorkspaceOp::Split { editor, .. } | WorkspaceOp::Reveal { editor, .. }) => Some(*editor),
                Self::Areas(WorkspaceOp::Select { instance, .. }) | Self::Focus { instance, .. } => Some(instance.editor),
                Self::Areas(WorkspaceOp::Move { what: Moving::Tab(instance), .. }) => Some(instance.editor),
                Self::Areas(WorkspaceOp::Move { what: Moving::Area(area), .. } | WorkspaceOp::Maximize { area: Some(area) }) => front(*area),
                _ => None,
            };
            if let Some(editor) = editor.filter(|e| !editors.contains(e)) {
                editors.push(editor);
            }
        }
        editors
    }

    /// Try this action on the probe. False means it is so already (a tab in front already, the
    /// workspace shown, the mode in force): no change, so no action, as the UI sends none.
    fn apply(&self, probe: &mut Workspaces, mode: &mut PlateMode) -> Result<bool, String> {
        match self {
            Self::Areas(op) => match probe.edit(*op) {
                Ok(()) => {}
                Err(OpError::Unchanged) => return Ok(false),
                Err(error) => return Err(op_error(error)),
            },
            Self::Switch(id) => {
                if probe.current == *id {
                    return Ok(false);
                }
                if !probe.switch(*id) {
                    return Err("no such workspace".into());
                }
                *mode = probe.current().mode.unwrap_or(*mode);
            }
            Self::List(op) => {
                let (changed, show) = probe.apply(op);
                if !changed {
                    return match op {
                        ListOp::Region { .. } => Ok(false),
                        ListOp::Dismiss | ListOp::Keep(_) => Err("no temporary layout is open".into()),
                        ListOp::UndoLayout(_) => Err("no layout change to undo".into()),
                        _ => Err("changed nothing".into()),
                    };
                }
                if let Some(id) = show {
                    probe.switch(id);
                    *mode = probe.current().mode.unwrap_or(*mode);
                } else if matches!(op, ListOp::Reset(id) if *id == probe.current) {
                    *mode = probe.current().mode.unwrap_or(*mode);
                }
            }
            Self::Focus { area, instance, thing, .. } => {
                if !shows_thing(instance.editor, thing) {
                    let (field, _) = thing.field();
                    return Err(format!("{} shows no {}: focus points the Schematic and the Board at nets, parts and pins, the 3D view at parts and a Scope at nets",
                        instance.editor.title(), if field == "focus" { "electronics" } else { field }));
                }
                match probe.edit(WorkspaceOp::Select { area: *area, instance: *instance }) {
                    Ok(()) | Err(OpError::Unchanged) => {}
                    Err(error) => return Err(op_error(error)),
                }
                return Ok(true);
            }
            Self::Mode(next) => {
                if !probe.current().shows(Family::Plate) {
                    return Err("the current workspace shows no Plate".into());
                }
                if *mode == *next {
                    return Ok(false);
                }
                *mode = *next;
            }
        }
        Ok(true)
    }
}

fn op_error(error: OpError) -> String {
    match error {
        OpError::NoArea(id) => format!("no area {} in this workspace", id.0),
        OpError::NoSplit(id) => format!("no split {} in this workspace", id.0),
        OpError::LastArea => "the last area cannot close".into(),
        OpError::OnScreen(id) => format!("area {} holds it already: swap or move it", id.0),
        OpError::Unchanged => "it is so already".into(),
    }
}

fn origin_word(origin: Origin) -> &'static str {
    match origin {
        Origin::Design => "design",
        Origin::Mechanics => "mechanics",
        Origin::Monitor => "monitor",
        Origin::Arm => "arm",
        Origin::Scan => "scan",
        Origin::Schematic => "schematic",
        Origin::SchematicBoard => "schematic_board",
        Origin::Board3d => "board_3d",
        Origin::DeviceScope => "device_scope",
        Origin::Firmware => "firmware",
        Origin::AllFour => "all_four",
        Origin::Bench => "bench",
    }
}

fn mode_word(mode: PlateMode) -> &'static str {
    match mode {
        PlateMode::Design => "design",
        PlateMode::Mechanics => "mechanics",
        PlateMode::Monitor => "monitor",
    }
}

fn side(word: &str) -> Option<Side> {
    match word {
        "left" => Some(Side::Left),
        "right" => Some(Side::Right),
        "up" => Some(Side::Up),
        "down" => Some(Side::Down),
        _ => None,
    }
}

fn instance_label(instance: Instance, name: &str) -> String {
    match instance.n {
        0 => name.to_owned(),
        n => format!("{name} {}", u64::from(n) + 1),
    }
}

fn instance_name(instance: Instance) -> String {
    instance_label(instance, instance.editor.name())
}

const STATEMENTS: &[(&str, &str)] = &[
    ("switch", "(ws)"),
    ("add", "(origin)"),
    ("rename", "(ws, \"name\")"),
    ("duplicate", "(ws)"),
    ("reset", "(ws)"),
    ("undo_layout", "(ws), the workspace optional (the shown one)"),
    ("delete", "(ws)"),
    ("reorder", "(ws, to)"),
    ("set", "(area, editor)"),
    ("swap", "(a, b)"),
    ("split", "(area, side, editor, share: x | first: px | last: px), the size optional (half)"),
    ("close", "(area)"),
    ("close_tab", "(area, editor)"),
    ("select", "(area, editor)"),
    ("region", "(area, tools: on|off, side: left|right), at least one of tools or side"),
    ("focus", "(area | editor, net: \"GND\" | part: \"R1\" | pin: \"U1.PB0\" | \"R1\", board: \"name\"), the board optional (where several have it)"),
    ("show", "(editor)"),
    ("move", "(area, to: area, at: side|center)"),
    ("move_tab", "(area, editor, to: area, at: side|center)"),
    ("resize", "(split, share: x | first: px | last: px)"),
    ("maximize", "(area)"),
    ("restore", "()"),
    ("mode", "(design|mechanics|monitor)"),
    ("temporary", "(\"note\", from: ws), from optional (the current one)"),
    ("dismiss", "()"),
    ("keep", "(\"name\"), the name optional (its note)"),
];

fn usage(name: Option<&str>) -> String {
    if let Some(&(name, args)) = STATEMENTS.iter().find(|&&(cmd, _)| Some(cmd) == name) {
        return format!("use {name}{args}");
    }
    let commands: Vec<_> = STATEMENTS.iter().map(|&(cmd, _)| cmd).collect();
    format!("workspace statements: {}", commands.join(", "))
}

/// A caption number, or an exact name among the offered workspaces, or `temp` (the temporary
/// layout). Two equal names can arise through Rename, so the caption number is needed to tell
/// those workspaces apart.
fn workspace(arg: &Node, source: &str, ws: &Workspaces, offered: &dyn Fn(&WorkspaceDef) -> bool) -> Result<WorkspaceId, String> {
    if arg.ident() == Some("temp") {
        return ws.temp_id().ok_or_else(|| "no temporary layout is open: temporary(\"note\") opens one".into());
    }
    let mut row = ws.list.iter().filter(|d| offered(d));
    if let Some(name) = arg.string() {
        let folded = name.to_lowercase();
        let mut matches = row.filter(|d| d.name.to_lowercase() == folded);
        let first = matches.next().ok_or_else(|| format!("no offered workspace named {}", quote(name)))?;
        if matches.next().is_some() {
            return Err(format!("several offered workspaces are named {}. Use a caption number", quote(name)));
        }
        return Ok(first.id);
    }
    let index = caption_index(arg, source)?;
    row.nth(index).map(|d| d.id).ok_or_else(|| format!("no offered workspace numbered {}", index + 1))
}

fn caption_index(arg: &Node, source: &str) -> Result<usize, String> {
    arg.text(source).parse::<usize>().ok().and_then(|n| n.checked_sub(1))
        .ok_or_else(|| "a caption number is a whole number starting at 1".into())
}

fn area_id(arg: &Node, source: &str) -> Result<AreaId, String> {
    arg.text(source).parse::<u32>().map(AreaId).map_err(|_| "expected a numeric area or split id".into())
}

/// Consume the arguments from the Splash call. Only a named argument can supply a share
/// or a drop target, and every statement must consume its whole argument list.
struct Args<'a> {
    source: &'a str,
    rest: &'a [Node],
}

impl<'a> Args<'a> {
    fn next(&mut self) -> Result<&'a Node, String> {
        let (first, rest) = self.rest.split_first().ok_or("missing argument")?;
        self.rest = rest;
        Ok(first)
    }

    fn area(&mut self) -> Result<AreaId, String> {
        area_id(self.next()?, self.source)
    }

    fn editor_name(&mut self) -> Result<String, String> {
        let arg = self.next()?;
        let next_ident = self.rest.first().filter(|n| n.ident().is_some() && n.span.start == arg.span.end);
        let name = if let Some(name) = arg.string().or_else(|| arg.ident()) {
            name
        } else if arg.number().is_some() && next_ident.is_some() {
            // A bare name starting with a digit (3D_view) spans two adjacent Splash nodes.
            let end = self.next()?.span.end;
            &self.source[arg.span.start..end]
        } else {
            return Err("expected an editor name or title".into());
        };
        Ok(name.replace('_', " "))
    }

    fn editor(&mut self) -> Result<Editor, String> {
        let name = self.editor_name()?;
        EDITORS.iter().find(|d| name.eq_ignore_ascii_case(d.name) || name.eq_ignore_ascii_case(d.title))
            .map(|d| d.editor).ok_or_else(|| format!("unknown editor {}. See the fab.workspace catalog", quote(&name)))
    }

    fn instance(&mut self, tree: &AreaTree, area: AreaId) -> Result<Instance, String> {
        let name = self.editor_name()?;
        let (tabs, _) = tree.stack_of(area).ok_or_else(|| op_error(OpError::NoArea(area)))?;
        tabs.iter().copied().find(|&i| {
            name.eq_ignore_ascii_case(&instance_name(i)) || name.eq_ignore_ascii_case(&instance_label(i, i.editor.title()))
        }).ok_or_else(|| {
            let names: Vec<_> = tabs.iter().map(|&i| quote(&instance_name(i))).collect();
            format!("area {} has no tab {}. It holds {}", area.0, quote(&name), names.join(", "))
        })
    }

    fn region(&mut self, tree: &AreaTree) -> Result<ListOp, String> {
        let area = self.area()?;
        let instance = tree.instance(area).ok_or_else(|| op_error(OpError::NoArea(area)))?;
        let editor = instance.editor;
        if !editor.has_tools() {
            return Err(format!("{} has no tool shelf. The editors with shelves are Plate and Schematic", editor.title()));
        }
        if self.rest.is_empty() {
            return Err("region needs tools or side".into());
        }
        let mut tools = None;
        let mut left = None;
        while !self.rest.is_empty() {
            let Kind::Entry { name, value, .. } = &self.next()?.kind else {
                return Err("expected tools: on|off or side: left|right".into());
            };
            match name.as_str() {
                "tools" if tools.is_none() => tools = Some(match value.ident() {
                    Some("on") => true,
                    Some("off") => false,
                    _ => return Err("tools takes on or off".into()),
                }),
                "side" if !editor.tools_move() => return Err(format!("{} has a fixed tool shelf. Only the Schematic shelf can change sides", editor.title())),
                "side" if left.is_none() => left = Some(match value.ident() {
                    Some("left") => true,
                    Some("right") => false,
                    _ => return Err("side takes left or right".into()),
                }),
                _ => return Err(format!("unexpected region argument {name}")),
            }
        }
        Ok(ListOp::Region { instance, tools, left })
    }

    /// A split's size as `fab.workspace` writes it: `share: x` the first part's share (between 0
    /// and 1), `first: px` / `last: px` the first or last part held at that many points.
    fn share(&mut self) -> Result<SplitterAlign, String> {
        let Kind::Entry { name, value, .. } = &self.next()?.kind else {
            return Err("expected share: x, first: px or last: px".into());
        };
        let number = value.text(self.source).parse::<f64>().ok().filter(|x| x.is_finite());
        match (name.as_str(), number) {
            ("share", Some(x)) if x > 0.0 && x < 1.0 => Ok(SplitterAlign::Weighted(x)),
            ("first", Some(px)) if px > 0.0 => Ok(SplitterAlign::FromA(px)),
            ("last", Some(px)) if px > 0.0 => Ok(SplitterAlign::FromB(px)),
            ("share", _) => Err("share is a number between 0 and 1".into()),
            ("first" | "last", _) => Err(format!("{name} is a size in points above 0")),
            _ => Err("expected share: x, first: px or last: px".into()),
        }
    }

    fn target(&mut self) -> Result<(AreaId, DropAt), String> {
        let mut to = None;
        let mut at = None;
        while !self.rest.is_empty() {
            let Kind::Entry { name, value, .. } = &self.next()?.kind else {
                return Err("expected to: area and at: side|center".into());
            };
            match name.as_str() {
                "to" if to.is_none() => to = Some(area_id(value, self.source)?),
                "at" if at.is_none() => at = Some(match value.ident() {
                    Some("center") => DropAt::Center,
                    word => word.and_then(side).map(DropAt::Side).ok_or("at takes left, right, up, down or center")?,
                }),
                _ => return Err(format!("unexpected drop argument {name}")),
            }
        }
        Ok((to.ok_or("missing to: area")?, at.ok_or("missing at: side|center")?))
    }
}

fn resolve(name: &str, mut args: Args<'_>, probe: &Workspaces, offered: &dyn Fn(&WorkspaceDef) -> bool) -> Result<WsEdit, String> {
    use WorkspaceOp::*;
    use WsEdit::{Areas, List};
    let source = args.source;
    let ws = |arg: &Node| workspace(arg, source, probe, offered);
    let edit = match name {
        "switch" => WsEdit::Switch(ws(args.next()?)?),
        "add" => {
            let word = args.next()?.ident();
            let origin = Origin::ALL.into_iter().find(|&o| Some(origin_word(o)) == word)
                .ok_or_else(|| format!("origins: {}", Origin::ALL.map(origin_word).join(", ")))?;
            let def = WorkspaceDef::of(origin, WorkspaceId(0));
            if !offered(&def) {
                let family = if def.shows(Family::Arm) { "Arm" } else if def.shows(Family::Scan) { "Scan" } else {
                    return Err("this default workspace is hidden".into());
                };
                return Err(format!("turn on View › Experimental › {family} first"));
            }
            List(ListOp::Add(origin))
        }
        "rename" => {
            let id = ws(args.next()?)?;
            let name = args.next()?.string().ok_or("expected a quoted workspace name")?;
            List(ListOp::Rename(id, name.to_owned()))
        }
        "duplicate" => List(ListOp::Duplicate(ws(args.next()?)?)),
        "reset" => List(ListOp::Reset(ws(args.next()?)?)),
        "undo_layout" => List(ListOp::UndoLayout(if args.rest.is_empty() { probe.current } else { ws(args.next()?)? })),
        "delete" => List(ListOp::Delete(ws(args.next()?)?)),
        "reorder" => {
            let id = ws(args.next()?)?;
            let to = caption_index(args.next()?, source)?;
            let mut row = probe.list.iter().enumerate().filter(|(_, d)| offered(d));
            let full = row.nth(to).map_or(probe.list.len() - 1, |(index, _)| index);
            List(ListOp::Move(id, full))
        }
        "set" => Areas(SetEditor { area: args.area()?, editor: args.editor()? }),
        "region" => List(args.region(&probe.current().tree)?),
        "swap" => Areas(Swap { a: args.area()?, b: args.area()? }),
        "split" => {
            let area = args.area()?;
            let side = args.next()?.ident().and_then(side).ok_or("side takes left, right, up or down")?;
            let editor = args.editor()?;
            let align = if args.rest.is_empty() { SplitterAlign::Weighted(0.5) } else { args.share()? };
            Areas(Split { area, side, editor, align })
        }
        "close" => Areas(Close { area: args.area()? }),
        "close_tab" | "select" => {
            let area = args.area()?;
            let instance = args.instance(&probe.current().tree, area)?;
            Areas(if name == "select" { Select { area, instance } } else { CloseTab { area, instance } })
        }
        "show" => Areas(Reveal { editor: args.editor()?, from: None }),
        "focus" => {
            // an area (its front tab), or a tab by name wherever an area holds it
            let tree = &probe.current().tree;
            let first = args.rest.first().ok_or("missing argument")?;
            let (area, instance) = if first.number().is_some() && first.text(source).parse::<u32>().is_ok() {
                let area = args.area()?;
                let (tabs, front) = tree.stack_of(area).ok_or_else(|| op_error(OpError::NoArea(area)))?;
                (area, tabs[front])
            } else {
                let name = args.editor_name()?;
                tree.areas().iter().find_map(|(area, _)| {
                    let (tabs, _) = tree.stack_of(*area)?;
                    tabs.iter().copied().find(|&i| name.eq_ignore_ascii_case(&instance_name(i)) || name.eq_ignore_ascii_case(&instance_label(i, i.editor.title())))
                        .map(|i| (*area, i))
                }).ok_or_else(|| format!("no area holds {}", quote(&name)))?
            };
            let arg = args.next()?;
            let thing = match &arg.kind {
                Kind::Entry { name, value, .. } => {
                    let text = value.string().ok_or_else(|| format!("{name}: a quoted name"))?.trim().to_owned();
                    match name.as_str() {
                        "net" => Thing::Net(text),
                        "part" => Thing::Part(text),
                        "pin" => Thing::Pin(text),
                        _ => return Err(format!("focus takes net:, part: or pin:, not {name}:")),
                    }
                }
                _ => Thing::Any(arg.string().ok_or("expected what to focus: net: \"GND\", part: \"R1\", pin: \"U1.PB0\" or a quoted name")?.trim().to_owned()),
            };
            let board = match args.rest.first().map(|n| &n.kind) {
                Some(Kind::Entry { name, value, .. }) if name == "board" => {
                    let board = value.string().ok_or("board: a quoted board name")?.trim().to_owned();
                    args.next()?;
                    Some(board)
                }
                _ => None,
            };
            WsEdit::Focus { area, instance, thing, board }
        }
        "move" | "move_tab" => {
            let area = args.area()?;
            let what = if name == "move_tab" {
                Moving::Tab(args.instance(&probe.current().tree, area)?)
            } else {
                Moving::Area(area)
            };
            let (to, at) = args.target()?;
            Areas(Move { what, to, at })
        }
        "resize" => Areas(Resize { split: args.area()?, align: args.share()? }),
        "maximize" => Areas(Maximize { area: Some(args.area()?) }),
        "restore" => Areas(Maximize { area: None }),
        "mode" => WsEdit::Mode(match args.next()?.ident() {
            Some("design") => PlateMode::Design,
            Some("mechanics") => PlateMode::Mechanics,
            Some("monitor") => PlateMode::Monitor,
            _ => return Err("unknown plate mode".into()),
        }),
        "temporary" => {
            let note = args.next()?.string().ok_or("expected a quoted note: what it shows")?.to_owned();
            let from = match args.rest.first().map(|n| &n.kind) {
                Some(Kind::Entry { name, value, .. }) if name == "from" => {
                    let from = ws(value)?;
                    args.next()?;
                    from
                }
                Some(_) => return Err("expected from: ws".into()),
                None => probe.current,
            };
            List(ListOp::Temporary { from, note, by: ShownBy::Ai })
        }
        "dismiss" => List(ListOp::Dismiss),
        "keep" => {
            let name = match args.rest.first() {
                Some(arg) => Some(arg.string().ok_or("expected a quoted workspace name")?.to_owned()),
                None => None,
            };
            if name.is_some() {
                args.next()?;
            }
            List(ListOp::Keep(name))
        }
        _ => return Err("unknown workspace statement".into()),
    };
    if !args.rest.is_empty() {
        return Err(format!("unexpected argument {}", args.rest[0].text(source)));
    }
    Ok(edit)
}

fn statement_error(n: usize, text: &str, start: usize, statement: &str, why: String) -> String {
    let (line, col) = Names::line_col_of(text, start);
    format!("statement {n} ({line}:{col}): {statement} — {why}")
}

/// Try the statements in order. On failure the caller discards the probe and the tracked mode.
pub fn run(
    text: &str,
    probe: &mut Workspaces,
    mode: &mut PlateMode,
    offered: &dyn Fn(&WorkspaceDef) -> bool,
) -> Result<Vec<WsEdit>, String> {
    probe.begin_group();
    let edits = run_statements(text, probe, mode, offered);
    probe.end_group();
    edits
}

fn run_statements(
    text: &str,
    probe: &mut Workspaces,
    mode: &mut PlateMode,
    offered: &dyn Fn(&WorkspaceDef) -> bool,
) -> Result<Vec<WsEdit>, String> {
    let mut edits = Vec::new();
    let mut count = 0;
    let mut read = |tree: syntax::Syntax, offset: usize, count: &mut usize| -> Result<(), String> {
        let source = tree.source();
        for stmt in tree.statements() {
            *count += 1;
            let fail = |why| statement_error(*count, text, offset + stmt.span.start, stmt.text(source), why);
            let (name, rest) = stmt.as_call().ok_or_else(|| fail(usage(None)))?;
            let edit = resolve(name, Args { source, rest }, probe, offered)
                .map_err(|why| fail(format!("{why}. {}", usage(Some(name)))))?;
            if edit.apply(probe, mode).map_err(fail)? {
                edits.push(edit);
            }
        }
        Ok(())
    };

    match syntax::parse(text) {
        Ok(tree) => read(tree, 0, &mut count)?,
        Err(error) => {
            // A syntax error leaves no tree. Use Splash's own tokens to recover complete
            // calls before it, so a bad later call still gets its number and source text.
            use makepad_script::tokenizer::{lex, ScriptTokenTag};
            let tokens = lex(text);
            let mut depth = 0usize;
            let calls = tokens.split_inclusive(|t| match t.token().kind() {
                ScriptTokenTag::OpenRound => { depth += 1; false }
                ScriptTokenTag::CloseRound => { depth = depth.saturating_sub(1); depth == 0 }
                ScriptTokenTag::Separator => depth == 0,
                _ => false,
            });
            for call in calls {
                let Some(first) = call.iter().find(|t| t.token().kind() != ScriptTokenTag::Separator) else { continue };
                let start = first.start();
                let last = call.last().unwrap();
                // An unfinished string can extend past its token's stored span.
                let end = if last.token().kind() == ScriptTokenTag::StringUnfinished { text.len() } else { last.end() };
                let source = &text[start..end];
                if error.at < end {
                    let (line, col) = Names::line_col_of(text, error.at);
                    let help = usage(Some(&text[first.span()]));
                    let why = format!("{} at {line}:{col}. {help}", error.message);
                    return Err(statement_error(count + 1, text, start, source, why));
                }
                let tree = syntax::parse(source).map_err(|err| statement_error(count + 1, text, start, source, err.message))?;
                read(tree, start, &mut count)?;
            }
            // The parser can also point just past the final token.
            return Err(statement_error(count + 1, text, error.at, &text[error.at..], error.message));
        }
    }
    if count == 0 {
        return Err(format!("no workspace statements. {}", usage(None)));
    }
    Ok(edits)
}

/// Write directly into the answer so each subtree and each editor name is appended once.
fn describe_tree(out: &mut String, tree: &AreaTree, rects: &HashMap<AreaId, Rect>, ws: &Workspaces) {
    match tree {
        AreaTree::Area { id, tabs, selected } => {
            write!(out, "{{area: {} editors: [", id.0).unwrap();
            for (index, &instance) in tabs.iter().enumerate() {
                if index != 0 {
                    out.push(' ');
                }
                out.push_str(&quote(&instance_name(instance)));
            }
            let front = tabs[*selected];
            write!(out, "] front: {}", quote(&instance_name(front))).unwrap();
            if front.editor.has_tools() {
                let state = ws.instance_state(front);
                out.push_str(if state.tools { " tools: on" } else { " tools: off" });
                if front.editor.tools_move() {
                    out.push_str(if state.tools_left { " side: left" } else { " side: right" });
                }
            }
            if let Some(rect) = rects.get(id) {
                write!(out, " rect: [{:.0}", rect.pos.x).unwrap();
                for point in [rect.pos.y, rect.size.x, rect.size.y] {
                    // Splash reads a minus after a number as subtraction unless separated.
                    if point.is_sign_negative() {
                        out.push(',');
                    }
                    write!(out, " {point:.0}").unwrap();
                }
                out.push(']');
            }
        }
        AreaTree::Split { id, axis, align, a, b } => {
            let axis = match axis { SplitterAxis::Horizontal => "across", SplitterAxis::Vertical => "down" };
            let (field, value) = match align {
                SplitterAlign::Weighted(x) => ("share", x),
                SplitterAlign::FromA(px) => ("first", px),
                SplitterAlign::FromB(px) => ("last", px),
            };
            write!(out, "{{split: {} axis: {axis} {field}: {value} a: ", id.0).unwrap();
            describe_tree(out, a, rects, ws);
            out.push_str(" b: ");
            describe_tree(out, b, rects, ws);
        }
    }
    out.push('}');
}

/// The offered caption row, active plate mode and current area tree as a Splash value.
pub fn describe(
    ws: &Workspaces,
    mode: PlateMode,
    rects: &[(AreaId, Rect)],
    catalog: bool,
    offered: &dyn Fn(&WorkspaceDef) -> bool,
) -> String {
    let mut out = String::from("{\n  workspaces: [");
    for (index, def) in ws.list.iter().filter(|d| offered(d)).enumerate() {
        if index != 0 {
            out.push(' ');
        }
        write!(out, "{{n: {} name: {} current: {}", index + 1, quote(&def.name), def.id == ws.current).unwrap();
        if let Some(origin) = def.origin {
            write!(out, " origin: {}", origin_word(origin)).unwrap();
        }
        if let Some(pin) = def.mode {
            write!(out, " mode: {}", mode_word(pin)).unwrap();
        }
        out.push('}');
    }
    out.push(']');
    if let Some(temp) = &ws.temp {
        let by = match temp.by { ShownBy::Ai => "ai", ShownBy::Person => "person" };
        write!(out, "\n  temporary: {{name: {} current: {} by: {by}}}", quote(&temp.def.name), temp.def.id == ws.current).unwrap();
    }
    write!(out, "\n  mode: {}\n  areas: ", mode_word(mode)).unwrap();
    let current = ws.current();
    let rects = rects.iter().copied().collect();
    describe_tree(&mut out, &current.tree, &rects, ws);
    if let Some(id) = current.maximized {
        write!(out, "\n  maximized: {}", id.0).unwrap();
    }
    if catalog {
        out.push_str("\n  editors: [");
        let mut separator = "";
        for (group, editors) in MENU {
            for &editor in editors {
                write!(out, "{separator}{{name: {} title: {} group: {} repeats: {}}}",
                    quote(editor.name()), quote(editor.title()), quote(group), editor.repeatable()).unwrap();
                separator = " ";
            }
        }
        out.push(']');
    }
    out.push_str("\n}");
    out
}

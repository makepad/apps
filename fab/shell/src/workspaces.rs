//! The workspaces (design FABWS, Blender's): the caption's row of named arrangements, the one
//! shown, and the file that keeps them on this machine. A workspace is a tree of areas, each a
//! stack of editors ([`AreaTree`]), and may pin a plate mode. Every change is an op on
//! [`Workspaces`] ([`ListOp`], or a [`WorkspaceOp`] on the shown tree); the app applies them in
//! `apply_core`, the shell lays out what the list says and the caption shows the list.
//!
//! Layout history takes one tree copy per recorded area edit. Deleting or dismissing a workspace
//! also releases its saved trees. Loading or saving the row costs O(workspaces × areas).
//! Nothing here runs per frame.

use crate::api::{Family, LinkHow, PlateMode, Workspace};
use crate::ui::areas::{AreaId, AreaTree, Instance, OpError, WorkspaceOp};
use crate::ui::editors::Editor;
use crate::ui::elec_layouts;
use makepad_widgets_core::makepad_micro_serde::*;
use makepad_widgets_core::*;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// A workspace's id: unique in the list and kept with it in the file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, SerRon, DeRon)]
pub struct WorkspaceId(pub u32);

/// The defaults a workspace can start as (and Reset puts back): the general five, then the
/// electronics seven.
#[derive(Clone, Copy, Debug, PartialEq, Eq, SerRon, DeRon)]
pub enum Origin {
    Design,
    Mechanics,
    Monitor,
    Arm,
    Scan,
    Schematic,
    SchematicBoard,
    Board3d,
    DeviceScope,
    Firmware,
    AllFour,
    Bench,
}

/// The recorder's area under the electronics defaults' areas, before its first draw fits it
/// (the strip's head and the recorder's single row).
const TRANSPORT_START_H: f64 = 77.0;
/// The Program's width beside the plate in a new Design, before the person sizes it.
pub const CODE_START_W: f64 = 440.0;

impl Origin {
    /// The caption's starting order.
    pub const ALL: [Origin; 12] = [
        Origin::Design, Origin::Mechanics, Origin::Monitor, Origin::Arm, Origin::Scan,
        Origin::Schematic, Origin::SchematicBoard, Origin::Board3d, Origin::DeviceScope, Origin::Firmware, Origin::AllFour, Origin::Bench,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Origin::Design => "Design",
            Origin::Mechanics => "Mechanics",
            Origin::Monitor => "Monitor",
            Origin::Arm => "Arm",
            Origin::Scan => "Scan",
            Origin::Schematic => "Schematic",
            Origin::SchematicBoard => "Schematic | Board",
            Origin::Board3d => "Board + 3D",
            Origin::DeviceScope => "Device + Scope",
            Origin::Firmware => "Firmware",
            Origin::AllFour => "All four",
            Origin::Bench => "Bench",
        }
    }

    /// One of the electronics seven (the `+` menu's second group).
    pub fn electronics(self) -> bool {
        self as usize >= Origin::Schematic as usize
    }

    /// The kind its arrangement is ([`WorkspaceDef::kind`]).
    pub fn kind(self) -> Workspace {
        match self {
            Origin::Design => Workspace::Design,
            Origin::Mechanics => Workspace::Mechanics,
            Origin::Monitor => Workspace::Monitor,
            Origin::Arm => Workspace::Arm,
            Origin::Scan => Workspace::Scan,
            _ => Workspace::Electronics,
        }
    }

    /// The default a seek of `kind` adds when no workspace answers it.
    pub fn of_kind(kind: Workspace) -> Origin {
        match kind {
            Workspace::Design => Origin::Design,
            Workspace::Mechanics => Origin::Mechanics,
            Workspace::Monitor => Origin::Monitor,
            Workspace::Arm => Origin::Arm,
            Workspace::Scan => Origin::Scan,
            Workspace::Electronics => Origin::Schematic,
        }
    }

    /// Its areas and the plate mode it pins. Design holds the Program beside the plate while the
    /// code toggle was last on (`view_prefs::load_code`, read on each call: a click's worth).
    pub fn tree(self) -> (AreaTree, Option<PlateMode>) {
        (self.arrangement(crate::view_prefs::load_code()), self.kind().pinned_mode())
    }

    /// Its areas, Design's with the Program beside the plate when `code` says so (on, and its
    /// width once the person sized it).
    fn arrangement(self, code: (bool, Option<f64>)) -> AreaTree {
        use AreaTree as T;
        use SplitterAlign::{FromA, FromB, Weighted};
        use SplitterAxis::{Horizontal, Vertical};
        let plate = || T::area(Editor::Viewport);
        let tree = match self {
            Origin::Design => {
                // left: the plates and the parts (in front), each part's features in its tree; right: the part, the
                // slicer, the job, the printer over the part's parameters (in front), the
                // overrides, strength and test prints; the program beside the plate
                let (code, width) = code;
                let centre = if code { T::split(Horizontal, FromB(width.unwrap_or(CODE_START_W)), plate(), T::area(Editor::Code)) } else { plate() };
                let left = T::stack(&[Editor::Plates, Editor::Parts], 1);
                let top = T::stack(&[Editor::Part, Editor::Slicer, Editor::Settings, Editor::Printer, Editor::Calibrate], 0);
                let below = T::stack(&[Editor::Parameters, Editor::Overrides, Editor::Strength, Editor::TestPrints], 0);
                let right = T::split(Vertical, Weighted(0.5), top, below);
                T::split(Horizontal, FromA(250.0), left, T::split(Horizontal, FromB(320.0), centre, right))
            }
            Origin::Mechanics => T::split(Horizontal, FromB(320.0), plate(), T::area(Editor::Mechanics)),
            Origin::Monitor => {
                let right = T::split(Vertical, FromA(340.0), T::area(Editor::Camera), T::stack(&[Editor::Printer, Editor::Calibrate], 0));
                T::split(Horizontal, FromB(460.0), plate(), right)
            }
            Origin::Arm => T::split(Horizontal, FromB(380.0), T::stack(&[Editor::ArmCell, Editor::ArmCompare], 0), T::area(Editor::Arm)),
            Origin::Scan => T::split(Horizontal, FromB(340.0), T::area(Editor::ScanView), T::area(Editor::Scan)),
            _ => {
                let at = self as usize - Origin::Schematic as usize;
                let tree = elec_layouts::defaults().swap_remove(at).1;
                let tree = if self == Origin::DeviceScope {
                    /// Room for the controls in Device + Scope, where the device is played.
                    const CONTROLS_START_H: f64 = 180.0;
                    T::split(Vertical, FromB(CONTROLS_START_H), tree, T::area(Editor::BoardControls))
                } else {
                    tree
                };
                // the recorder under the areas, at its own height (Fable's defaults table)
                T::split(Vertical, FromB(TRANSPORT_START_H), tree, T::area(Editor::Transport))
            }
        };
        tree.numbered()
    }
}

/// The editors near `editor` in the first default that holds it, nearest first
/// ([`AreaTree::nearby`]); none when no default does. A Reveal of an editor no area holds puts it
/// with the nearest of them on screen. O(defaults x tabs), on a reveal; no file read.
pub fn neighbours(editor: Editor) -> Vec<Editor> {
    Origin::ALL.iter().find_map(|o| o.arrangement((false, None)).nearby(editor)).unwrap_or_default()
}

/// One workspace of the caption's row.
#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceDef {
    pub id: WorkspaceId,
    pub name: String,
    /// Its areas; every change is a [`WorkspaceOp`].
    pub tree: AreaTree,
    /// The plate mode choosing it puts the plate in (none: the plate keeps its mode).
    pub mode: Option<PlateMode>,
    /// The default it started as: Reset puts it back.
    pub origin: Option<Origin>,
    /// Moves with each change of the arrangement's shape (not a sash the dock moved): the shell
    /// lays the workspace out again when it moves.
    pub shape: u64,
    /// The area filling the workspace alone, if one is maximized (kept for the session).
    pub maximized: Option<AreaId>,
}

impl WorkspaceDef {
    pub(crate) fn of(origin: Origin, id: WorkspaceId) -> WorkspaceDef {
        let (tree, mode) = origin.tree();
        WorkspaceDef { id, name: origin.name().to_string(), tree, mode, origin: Some(origin), shape: 0, maximized: None }
    }

    /// The kind it is ([`Workspaces::seek`], the menus, the keys): with the plate, its pinned
    /// mode's (Design without one); else the family it holds, electronics first. O(tabs).
    pub fn kind(&self) -> Workspace {
        if self.shows(Family::Plate) {
            return match self.mode {
                Some(PlateMode::Mechanics) => Workspace::Mechanics,
                Some(PlateMode::Monitor) => Workspace::Monitor,
                _ => Workspace::Design,
            };
        }
        [Workspace::Electronics, Workspace::Arm, Workspace::Scan].into_iter().find(|k| self.shows(k.family())).unwrap_or(Workspace::Design)
    }

    /// One of its areas holds an editor of `family`, in front or not. O(tabs), no allocation.
    pub fn shows(&self, family: Family) -> bool {
        self.tree.any_instance(&|i| i.editor.family() == Some(family))
    }
}

/// What an editor instance keeps beside its body (design Q1 §1): its regions, and a Plate
/// view's link and mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstanceState {
    /// Its tool shelf shows (the Plate's tool rails, the Schematic's Library).
    pub tools: bool,
    /// Its shelf sits on the left (the Library's side; the Plate's rails do not move).
    pub tools_left: bool,
    /// A Plate view's camera follows another Plate view's, by that one's instance number
    /// (View › Underside view: Flipped; View › Traced view: Synced).
    pub link: Option<(LinkHow, u32)>,
    /// A Plate view traces its picture.
    pub traced: bool,
}

impl Default for InstanceState {
    fn default() -> Self {
        Self { tools: true, tools_left: false, link: None, traced: false }
    }
}

/// The views View › Underside view and View › Traced view open beside a Plate view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlatePair {
    /// The plate from below: Flipped to its source (See through: Through).
    Underside,
    /// The same view traced: Synced to its source.
    Traced,
}

impl PlatePair {
    /// A Plate view's state as this pair of Plate view `source`.
    fn state(self, source: u32) -> InstanceState {
        let (how, traced) = match self {
            PlatePair::Underside => (LinkHow::Flipped, false),
            PlatePair::Traced => (LinkHow::Synced, true),
        };
        InstanceState { tools: false, tools_left: false, link: Some((how, source)), traced }
    }

    /// A Plate view in `state` is this pair of Plate view `source`.
    fn holds(self, state: &InstanceState, source: u32) -> bool {
        match (self, state.link) {
            (PlatePair::Underside, Some((LinkHow::Flipped | LinkHow::Through, s))) => s == source,
            (PlatePair::Traced, Some((LinkHow::Synced, s))) => s == source && state.traced,
            _ => false,
        }
    }
}

fn link_name(how: LinkHow) -> &'static str {
    match how {
        LinkHow::Synced => "synced",
        LinkHow::Flipped => "flipped",
        LinkHow::Through => "through",
    }
}

fn link_of(name: &str) -> Option<LinkHow> {
    [LinkHow::Synced, LinkHow::Flipped, LinkHow::Through].into_iter().find(|h| link_name(*h) == name)
}

impl InstanceState {
    /// An instance's shelf before anyone folds or opens it: a Plate view past the first opens
    /// with its rails folded, every other one with its shelf showing.
    pub fn default_for(instance: Instance) -> Self {
        Self { tools: !(instance.editor == Editor::Viewport && instance.n > 0), ..Self::default() }
    }
}

/// A change to the workspace list or a window-global editor instance.
#[derive(Clone, Debug, PartialEq)]
pub enum ListOp {
    Rename(WorkspaceId, String),
    Region { instance: Instance, tools: Option<bool>, left: Option<bool> },
    /// View › Underside view or View › Traced view of Plate view `source` (its instance number)
    /// in the shown workspace. Off when a Plate view there is that pair: its area closes and its
    /// state goes. On: the source's area splits right in half with a fresh Plate view, its rails
    /// folded, linked to the source (Flipped; Synced and traced). On, off, on gives the same
    /// areas.
    Pair { source: u32, pair: PlatePair },
    /// A copy right after it, shown.
    Duplicate(WorkspaceId),
    /// Its default's arrangement and mode back (only with an origin).
    Reset(WorkspaceId),
    /// Restore its most recent layout snapshot.
    UndoLayout(WorkspaceId),
    /// Gone from the row (never the last one); the one beside it shows if it showed.
    Delete(WorkspaceId),
    /// To place `to` in the row.
    Move(WorkspaceId, usize),
    /// A default at the end of the row, shown.
    Add(Origin),
    /// The temporary layout as a copy of `from` titled `note`, shown. One open already takes the
    /// copy in place (its tab, its id); Back then returns to where the person was before it.
    Temporary { from: WorkspaceId, note: String, by: ShownBy },
    /// The temporary layout gone; the workspace it came over shows again if it was showing.
    Dismiss,
    /// The temporary layout into the row as a workspace at its end, named so (else its note).
    Keep(Option<String>),
}

/// Who opened the temporary layout: the AI showing something, or the person (`+`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShownBy {
    Ai,
    Person,
}

/// The temporary layout (design FABWS 4): a full workspace held outside the row, one at a time,
/// to show something without touching the person's workspaces. Never saved: Keep moves it into
/// the row, Back (or a restart) drops it.
#[derive(Clone, Debug, PartialEq)]
pub struct Temporary {
    /// Its areas and mode; its name is the note (the tab's title).
    pub def: WorkspaceDef,
    /// The workspace that showed before it: Back shows it again.
    pub return_to: WorkspaceId,
    pub by: ShownBy,
}

type LayoutSnapshot = (AreaTree, Option<AreaId>);

/// The caption's row and the one shown (one of the row, or the temporary layout). The row is
/// never empty.
#[derive(Clone, Debug)]
pub struct Workspaces {
    pub list: Vec<WorkspaceDef>,
    pub current: WorkspaceId,
    /// Only instances with a shelf setting of their own.
    instances: HashMap<Instance, InstanceState>,
    /// Moves when an instance's state changes, independently of the layout and caption.
    pub instance_rev: u64,
    /// The temporary layout, while one is open (shown or not).
    pub temp: Option<Temporary>,
    temp_gen: u64,
    next: u32,
    /// Session history. A tool probe shares the immutable snapshots and pops its own stacks.
    history: HashMap<WorkspaceId, Vec<Arc<LayoutSnapshot>>>,
    history_rev: u64,
    /// The workspaces that already have a snapshot for the open group.
    group: Option<HashSet<WorkspaceId>>,
    /// Moves with each change of the list (a name, the order, one added or gone, the one shown):
    /// the caption syncs when it moves.
    pub rev: u64,
    /// A sash moved since the file was last written (it is written once the hand lets go).
    pub unsaved: bool,
}

impl Default for Workspaces {
    fn default() -> Self {
        let list: Vec<WorkspaceDef> = Origin::ALL.iter().enumerate().map(|(i, o)| WorkspaceDef::of(*o, WorkspaceId(i as u32 + 1))).collect();
        Workspaces { current: list[0].id, next: list.len() as u32 + 1, list, instances: HashMap::new(), instance_rev: 0,
            temp: None, temp_gen: 0, history: HashMap::new(), history_rev: 0, group: None, rev: 0, unsaved: false }
    }
}

impl Workspaces {
    /// The AI's temporary layout shows fresh Plate views, each as the one it copies (its shelf,
    /// its link and mode): what the AI frames and turns there never moves the person's Plate
    /// views, and Back lets go of them. O(workspaces x tabs).
    fn fresh_plates(&mut self, tree: &mut AreaTree) {
        let mut taken = self.plates_but(WorkspaceId(u32::MAX));
        let mine: Vec<(AreaId, Instance)> = tree.instances().into_iter().filter(|(_, i)| i.editor == Editor::Viewport).collect();
        let mut map: Vec<(u32, u32)> = Vec::new();
        for (_, old) in &mine {
            let n = (0..).find(|n| !taken.contains(&Instance { editor: Editor::Viewport, n: *n })).unwrap_or(0);
            taken.push(Instance { editor: Editor::Viewport, n });
            map.push((old.n, n));
        }
        for &(area, old) in &mine {
            let Some(&(_, n)) = map.iter().find(|(o, _)| *o == old.n) else { continue };
            let fresh = Instance { editor: Editor::Viewport, n };
            if let Some((tabs, _)) = tree.stack_mut(area) {
                for tab in tabs.iter_mut().filter(|t| **t == old) {
                    *tab = fresh;
                }
            }
            let mut state = self.instance_state(old);
            state.link = state.link.map(|(how, source)| (how, map.iter().find(|(o, _)| *o == source).map_or(source, |(_, n)| *n)));
            if state == InstanceState::default_for(fresh) {
                self.instances.remove(&fresh);
            } else {
                self.instances.insert(fresh, state);
            }
        }
    }

    /// The temporary layout `tree` went: the state of its Plate views no workspace holds goes too
    /// (the AI's fresh ones), so the file is as it was. O(workspaces x tabs).
    fn forget_plates_of(&mut self, tree: &AreaTree) {
        let held = self.plates_but(WorkspaceId(u32::MAX));
        for (_, i) in tree.instances() {
            if i.editor == Editor::Viewport && !held.contains(&i) && self.instances.remove(&i).is_some() {
                self.instance_rev += 1;
            }
        }
    }

    /// The Plate views every workspace but `but` holds (the row and the temporary layout).
    /// O(workspaces x tabs).
    fn plates_but(&self, but: WorkspaceId) -> Vec<Instance> {
        let mut out = Vec::new();
        for d in self.list.iter().chain(self.temp.iter().map(|t| &t.def)).filter(|d| d.id != but) {
            for (_, i) in d.tree.instances() {
                if i.editor == Editor::Viewport && !out.contains(&i) {
                    out.push(i);
                }
            }
        }
        out
    }

    /// The shown workspace's Plate view that is `pair` of Plate view `source`, and its area.
    pub fn paired(&self, source: u32, pair: PlatePair) -> Option<(AreaId, Instance)> {
        self.current().tree.instances().into_iter()
            .find(|(_, i)| i.editor == Editor::Viewport && pair.holds(&self.instance_state(*i), source))
    }

    /// A Plate view's link and mode changed on its pane (its chip, See through): kept with it.
    pub fn set_plate_view(&mut self, instance: Instance, link: Option<(LinkHow, u32)>, traced: bool) {
        let old = self.instance_state(instance);
        let state = InstanceState { link, traced, ..old };
        if state == old {
            return;
        }
        if state == InstanceState::default_for(instance) {
            self.instances.remove(&instance);
        } else {
            self.instances.insert(instance, state);
        }
        self.instance_rev += 1;
    }

    pub fn instance_state(&self, instance: Instance) -> InstanceState {
        self.instances.get(&instance).copied().unwrap_or_else(|| InstanceState::default_for(instance))
    }

    pub fn current(&self) -> &WorkspaceDef {
        self.get(self.current).unwrap_or(&self.list[0])
    }

    /// A workspace of the row, or the temporary layout.
    pub fn get(&self, id: WorkspaceId) -> Option<&WorkspaceDef> {
        self.list.iter().find(|d| d.id == id).or_else(|| self.temp.as_ref().map(|t| &t.def).filter(|d| d.id == id))
    }

    fn get_mut(&mut self, id: WorkspaceId) -> Option<&mut WorkspaceDef> {
        if let Some(at) = self.position(id) {
            return Some(&mut self.list[at]);
        }
        self.temp.as_mut().map(|t| &mut t.def).filter(|d| d.id == id)
    }

    /// The temporary layout is the one shown.
    pub fn temp_shown(&self) -> bool {
        self.temp.as_ref().is_some_and(|t| t.def.id == self.current)
    }

    /// The temporary layout's id, while one is open.
    pub fn temp_id(&self) -> Option<WorkspaceId> {
        self.temp.as_ref().map(|t| t.def.id)
    }

    /// Moves each time a temporary layout opens, including replacements on the same tab.
    pub fn temp_gen(&self) -> u64 {
        self.temp_gen
    }

    pub fn position(&self, id: WorkspaceId) -> Option<usize> {
        self.list.iter().position(|d| d.id == id)
    }

    pub fn begin_group(&mut self) {
        self.group = Some(HashSet::new());
    }

    pub fn end_group(&mut self) {
        self.group = None;
    }

    pub fn can_undo(&self, id: WorkspaceId) -> bool {
        self.history_len(id) > 0
    }

    pub fn history_len(&self, id: WorkspaceId) -> usize {
        self.history.get(&id).map_or(0, Vec::len)
    }

    /// Moves when a snapshot is pushed or popped, or a workspace takes its history with it.
    pub fn history_rev(&self) -> u64 {
        self.history_rev
    }

    fn clear_history(&mut self, id: WorkspaceId) {
        if self.history.remove(&id).is_some_and(|stack| !stack.is_empty()) {
            self.history_rev += 1;
        }
    }

    fn needs_snapshot(&self, id: WorkspaceId, changes_shape: bool) -> bool {
        self.group.as_ref().map_or(changes_shape, |group| !group.contains(&id))
    }

    fn push_snapshot(&mut self, id: WorkspaceId, snapshot: LayoutSnapshot) {
        self.history.entry(id).or_default().push(Arc::new(snapshot));
        self.history_rev += 1;
        if let Some(group) = &mut self.group {
            group.insert(id);
        }
    }

    fn fresh_id(&mut self) -> WorkspaceId {
        let id = WorkspaceId(self.next);
        self.next += 1;
        id
    }

    /// A name no workspace has: `name`, else `name 2`, `name 3` ...
    fn free_name(&self, name: &str) -> String {
        let base = name.trim_end_matches(|c: char| c.is_ascii_digit()).trim_end();
        let base = if base.is_empty() { name } else { base };
        (1..).map(|n| if n == 1 { name.to_string() } else { format!("{base} {n}") })
            .find(|candidate| self.list.iter().all(|d| d.name != *candidate))
            .unwrap_or_default()
    }

    /// The first workspace that answers `kind` among those `offered`, the shown one first: a
    /// mode's kind needs that pinned mode with a plate, Arm, Scan and Electronics their family.
    pub fn seek(&self, kind: Workspace, offered: impl Fn(&WorkspaceDef) -> bool) -> Option<WorkspaceId> {
        let answers = |d: &WorkspaceDef| match kind.pinned_mode() {
            Some(mode) => d.mode == Some(mode) && d.shows(Family::Plate),
            None => d.shows(kind.family()),
        };
        let current = self.current();
        if answers(current) && offered(current) {
            return Some(current.id);
        }
        self.list.iter().find(|d| answers(d) && offered(d)).map(|d| d.id)
    }

    /// Shows `id`; false when it is not in the list or shows already.
    pub fn switch(&mut self, id: WorkspaceId) -> bool {
        if self.current == id || self.get(id).is_none() {
            return false;
        }
        self.current = id;
        self.rev += 1;
        true
    }

    /// Applies a list op; the workspace to show after it when that changes (`Some`), and whether
    /// anything changed. Of the row's ops the temporary layout takes Rename (which keeps it under
    /// that name), Delete (Back), Reset and UndoLayout; it has no place to move to or duplicate at.
    pub fn apply(&mut self, op: &ListOp) -> (bool, Option<WorkspaceId>) {
        let temp = self.temp_id();
        let changed = match op {
            ListOp::Region { instance, tools, left } => {
                let old = self.instance_state(*instance);
                let state = InstanceState { tools: tools.unwrap_or(old.tools), tools_left: left.unwrap_or(old.tools_left), ..old };
                if state == old {
                    return (false, None);
                }
                if state == InstanceState::default_for(*instance) {
                    self.instances.remove(instance);
                } else {
                    self.instances.insert(*instance, state);
                }
                self.instance_rev += 1;
                return (true, None);
            }
            ListOp::Pair { source, pair } => {
                if let Some((area, instance)) = self.paired(*source, *pair) {
                    if self.edit(WorkspaceOp::Close { area }).is_err() {
                        return (false, None);
                    }
                    self.instances.remove(&instance);
                    self.instance_rev += 1;
                    return (true, None);
                }
                let of = Instance { editor: Editor::Viewport, n: *source };
                let Some(area) = self.current().tree.instances().into_iter().find(|(_, i)| *i == of).map(|(area, _)| area) else {
                    return (false, None);
                };
                let had = self.current().tree.instances();
                let split = WorkspaceOp::Split { area, side: crate::ui::areas::Side::Right, editor: Editor::Viewport, align: SplitterAlign::Weighted(0.5) };
                if self.edit(split).is_err() {
                    return (false, None);
                }
                let Some((_, fresh)) = self.current().tree.instances().into_iter().find(|(_, i)| i.editor == Editor::Viewport && !had.iter().any(|(_, h)| h == i)) else {
                    return (true, None);
                };
                self.instances.insert(fresh, pair.state(*source));
                self.instance_rev += 1;
                return (true, None);
            }
            ListOp::Rename(id, name) if Some(*id) == temp && !name.trim().is_empty() => return self.apply(&ListOp::Keep(Some(name.clone()))),
            ListOp::Delete(id) if Some(*id) == temp => return self.apply(&ListOp::Dismiss),
            ListOp::Temporary { from, note, by } => {
                let Some(source) = self.get(*from) else { return (false, None) };
                let mut def = source.clone();
                let note = note.trim();
                if !note.is_empty() {
                    def.name = note.to_string();
                }
                // one open takes the copy in place; Back returns to where the person is now
                // unless that is the temporary layout itself
                let (id, return_to, shape) = match &self.temp {
                    Some(t) => (t.def.id, if self.current == t.def.id { t.return_to } else { self.current }, t.def.shape.max(def.shape) + 1),
                    None => (self.fresh_id(), self.current, def.shape),
                };
                def.id = id;
                def.shape = shape;
                if *by == ShownBy::Ai {
                    self.fresh_plates(&mut def.tree);
                }
                if let Some(old) = self.temp.as_ref().map(|t| t.def.tree.clone()) {
                    // replaced in place: what its own Plate views kept goes with them
                    let mut held = self.plates_but(id);
                    held.extend(def.tree.instances().into_iter().map(|(_, i)| i));
                    for (_, i) in old.instances() {
                        if i.editor == Editor::Viewport && !held.contains(&i) {
                            self.instances.remove(&i);
                        }
                    }
                }
                // a new temporary layout: the one it replaces in place takes its history with it
                self.clear_history(id);
                self.temp = Some(Temporary { def, return_to, by: *by });
                self.temp_gen += 1;
                self.rev += 1;
                return (true, Some(id));
            }
            ListOp::Dismiss => {
                let Some(t) = self.temp.take() else { return (false, None) };
                self.clear_history(t.def.id);
                self.forget_plates_of(&t.def.tree);
                self.rev += 1;
                if self.current != t.def.id {
                    return (true, None);
                }
                // what it came over, else (deleted meanwhile) the first
                let back = if self.position(t.return_to).is_some() { t.return_to } else { self.list[0].id };
                return (true, Some(back));
            }
            ListOp::Keep(name) => {
                let Some(t) = self.temp.take() else { return (false, None) };
                let mut def = t.def;
                let name = name.as_deref().map(str::trim).filter(|n| !n.is_empty()).map(str::to_string).unwrap_or_else(|| def.name.clone());
                def.name = self.free_name(&name);
                self.list.push(def);
                self.rev += 1;
                return (true, None);
            }
            ListOp::Rename(id, name) => {
                let name = name.trim().to_string();
                match self.get_mut(*id) {
                    Some(d) if !name.is_empty() && d.name != name => {
                        d.name = name;
                        true
                    }
                    _ => false,
                }
            }
            ListOp::Duplicate(id) => {
                let Some(at) = self.position(*id) else { return (false, None) };
                let mut copy = self.list[at].clone();
                copy.id = self.fresh_id();
                copy.name = self.free_name(&copy.name);
                let shown = copy.id;
                self.list.insert(at + 1, copy);
                self.rev += 1;
                return (true, Some(shown));
            }
            ListOp::Reset(id) => {
                let record = self.needs_snapshot(*id, true);
                let Some(d) = self.get_mut(*id) else { return (false, None) };
                let Some(origin) = d.origin else { return (false, None) };
                let (tree, mode) = origin.tree();
                if d.tree == tree && d.mode == mode && d.maximized.is_none() {
                    return (false, None);
                }
                let before = (std::mem::replace(&mut d.tree, tree), d.maximized.take());
                d.mode = mode;
                d.shape += 1;
                if record {
                    self.push_snapshot(*id, before);
                }
                true
            }
            ListOp::UndoLayout(id) => {
                let Some(snapshot) = self.history.get_mut(id).and_then(Vec::pop) else { return (false, None) };
                self.history_rev += 1;
                let Some(d) = self.get_mut(*id) else { return (false, None) };
                (d.tree, d.maximized) = Arc::unwrap_or_clone(snapshot);
                d.shape += 1;
                true
            }
            ListOp::Delete(id) => {
                let Some(at) = self.position(*id).filter(|_| self.list.len() > 1) else { return (false, None) };
                self.list.remove(at);
                self.clear_history(*id);
                self.rev += 1;
                // the shown one deleted: the one before it shows, as in Blender (the first's: the next)
                if self.current == *id {
                    let next = self.list[at.saturating_sub(1)].id;
                    return (true, Some(next));
                }
                return (true, None);
            }
            ListOp::Move(id, to) => {
                let Some(at) = self.position(*id) else { return (false, None) };
                let to = (*to).min(self.list.len() - 1);
                if at == to {
                    return (false, None);
                }
                let d = self.list.remove(at);
                self.list.insert(to, d);
                true
            }
            ListOp::Add(origin) => {
                let id = self.fresh_id();
                let mut d = WorkspaceDef::of(*origin, id);
                d.name = self.free_name(&d.name);
                self.list.push(d);
                self.rev += 1;
                return (true, Some(id));
            }
        };
        if changed {
            self.rev += 1;
        }
        (changed, None)
    }

    /// A change to the shown workspace's areas. A sash moved or a tab pressed leaves its shape as
    /// it is (the dock shows it already) and is written once the hand lets go; every other op
    /// moves it, so the shell lays the areas out again.
    pub fn edit(&mut self, op: WorkspaceOp) -> Result<(), OpError> {
        let current = self.current;
        let changes_shape = !matches!(op, WorkspaceOp::Resize { .. } | WorkspaceOp::Select { .. });
        let record = self.needs_snapshot(current, changes_shape);
        let grouped = self.group.is_some();
        // a new Plate view is one no other workspace holds, and starts with no state of its own
        let taken = self.plates_but(current);
        let plates = |tree: &AreaTree| tree.instances().into_iter().map(|(_, i)| i).filter(|i| i.editor == Editor::Viewport).collect::<Vec<_>>();
        let Some(d) = self.get_mut(current) else { return Err(OpError::Unchanged) };
        let had = plates(&d.tree);
        let before = record.then(|| (d.tree.clone(), d.maximized));
        if let WorkspaceOp::Maximize { area } = op {
            if let Some(area) = area.filter(|a| d.tree.stack_of(*a).is_none()) {
                return Err(OpError::NoArea(area));
            }
            if d.maximized == area {
                return Err(OpError::Unchanged);
            }
            d.maximized = area;
        } else {
            d.tree.apply_taking(op, &taken)?;
            // an area that went is no longer maximized
            if d.maximized.is_some_and(|a| d.tree.stack_of(a).is_none()) {
                d.maximized = None;
            }
        }
        if changes_shape {
            d.shape += 1;
        }
        let added: Vec<Instance> = plates(&d.tree).into_iter().filter(|i| !had.contains(i)).collect();
        // A resize can succeed with the sash already at the requested position.
        if let Some(snapshot) = before.filter(|(tree, maximized)| !grouped || *maximized != d.maximized || *tree != d.tree) {
            self.push_snapshot(current, snapshot);
        }
        if !changes_shape {
            // the temporary layout is not in the file
            self.unsaved |= !self.temp_shown();
        }
        for i in added {
            if self.instances.remove(&i).is_some() {
                self.instance_rev += 1;
            }
        }
        Ok(())
    }
}

/// The file's form of the list: the defs without the session's shape counters.
#[derive(SerRon, DeRon)]
struct WorkspacesFile {
    current: WorkspaceId,
    list: Vec<WorkspaceFileDef>,
    instances: Option<Vec<InstanceFileState>>,
}

#[derive(SerRon, DeRon)]
struct InstanceFileState {
    instance: Instance,
    tools: bool,
    tools_left: bool,
    /// A Plate view's link (`synced`, `flipped`, `through`) to Plate view `source`.
    link: Option<String>,
    source: Option<u32>,
    traced: Option<bool>,
}

#[derive(SerRon, DeRon)]
struct WorkspaceFileDef {
    id: WorkspaceId,
    name: String,
    /// Its areas when the person changed them from its default's; none: its default's, so it
    /// follows the defaults as they improve.
    tree: Option<AreaTree>,
    mode: Option<PlateMode>,
    origin: Option<Origin>,
}

impl Workspaces {
    /// The list as the file keeps it: a workspace's areas only when they are not its default's.
    /// The temporary layout is not kept: shown, the file names the workspace it came over.
    pub fn to_ron(&self) -> String {
        let current = match &self.temp {
            Some(t) if t.def.id == self.current => t.return_to,
            _ => self.current,
        };
        WorkspacesFile {
            current,
            list: self.list.iter().map(|d| WorkspaceFileDef {
                id: d.id, name: d.name.clone(), mode: d.mode, origin: d.origin,
                tree: d.origin.is_none_or(|o| o.tree().0 != d.tree).then(|| d.tree.clone()),
            }).collect(),
            instances: (!self.instances.is_empty()).then(|| {
                let mut entries: Vec<_> = self.instances.iter().map(|(&instance, state)| InstanceFileState {
                    instance, tools: state.tools, tools_left: state.tools_left,
                    link: state.link.map(|(how, _)| link_name(how).to_string()),
                    source: state.link.map(|(_, source)| source),
                    traced: state.traced.then_some(true),
                }).collect();
                entries.sort_by_key(|e| (e.instance.editor as usize, e.instance.n));
                entries
            }),
        }.serialize_ron()
    }

    /// The list from the file's text, a workspace without areas of its own on its default's;
    /// none when it does not read as a list (or is empty, or an area in it holds no editor, or a
    /// workspace has neither areas nor a default).
    pub fn from_ron(text: &str) -> Option<Workspaces> {
        let file = WorkspacesFile::deserialize_ron(text).ok()?;
        let list: Vec<WorkspaceDef> = file.list.into_iter().map(|d| {
            let tree = match (d.tree, d.origin) {
                (Some(tree), _) => tree.numbered(),
                (None, Some(origin)) => origin.tree().0,
                (None, None) => return None,
            };
            tree.whole().then_some(WorkspaceDef { id: d.id, name: d.name, mode: d.mode, origin: d.origin, shape: 0, tree, maximized: None })
        }).collect::<Option<_>>()?;
        if list.is_empty() {
            return None;
        }
        let next = list.iter().map(|d| d.id.0).max().unwrap_or(0) + 1;
        let current = if list.iter().any(|d| d.id == file.current) { file.current } else { list[0].id };
        let instances = file.instances.unwrap_or_default().into_iter().filter_map(|entry| {
            let link = entry.link.as_deref().and_then(link_of).zip(entry.source);
            let state = InstanceState { tools: entry.tools, tools_left: entry.tools_left, link, traced: entry.traced.unwrap_or(false) };
            (state != InstanceState::default_for(entry.instance)).then_some((entry.instance, state))
        }).collect();
        Some(Workspaces { list, current, next, instances, instance_rev: 0,
            temp: None, temp_gen: 0, history: HashMap::new(), history_rev: 0, group: None, rev: 0, unsaved: false })
    }

    /// The person's list on this machine, else the defaults (a file that does not read says so
    /// in the log and leaves the defaults).
    pub fn load() -> Workspaces {
        let Some(path) = crate::view_prefs::workspaces_path() else { return Workspaces::default() };
        let Ok(text) = std::fs::read_to_string(&path) else { return Workspaces::default() };
        Workspaces::from_ron(&text).unwrap_or_else(|| {
            log!("fab: {} does not read as workspaces; the default ones show", path.display());
            Workspaces::default()
        })
    }

    /// Keeps the list for the next start (written whole, then moved over the old file).
    pub fn save(&mut self) {
        self.unsaved = false;
        let Some(path) = crate::view_prefs::workspaces_path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let fresh = path.with_extension("ron.new");
        if std::fs::write(&fresh, self.to_ron()).is_ok() {
            let _ = std::fs::rename(&fresh, &path);
        }
    }
}

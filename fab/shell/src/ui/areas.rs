//! A workspace's areas (design FABWS; an area is a stack of editors like one of our Dock's tab
//! sets, user 11-20: "like we had with our dock"): a binary tree of splits whose leaves are areas,
//! each holding one or more editor instances, one of them in front. Every change to it is a
//! [`WorkspaceOp`] applied by [`AreaTree::apply`], whoever asks for it (an area's menu, a drag, a
//! key, the AI), and the dock's map is derived from the tree ([`present`]) and loaded keeping the
//! editors' bodies, so the dock never changes a layout by itself. Read back from the dock: a sash
//! moved by hand ([`WorkspaceOp::Resize`]) and a tab pressed ([`WorkspaceOp::Select`]).
//!
//! Costs: an op is O(areas + tabs), on a click, a key, a drop or a tool call; deriving a map is
//! O(areas + tabs), per load (a drag's preview loads one each time its target changes). Nothing
//! here runs per frame.

use crate::ui::editors::Editor;
use makepad_widgets_core::makepad_micro_serde::*;
use makepad_widgets_core::*;
use makepad_widgets_data::dock::DockItem;
use std::collections::HashMap;

pub type DockMap = HashMap<LiveId, DockItem>;

/// A node of an area tree, an area or a split: a small number, unique in its tree and kept as
/// long as the node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, SerRon, DeRon)]
pub struct AreaId(pub u32);

impl AreaId {
    /// The dock's id for this node as an area (its tab set).
    pub fn area_live_id(self) -> LiveId {
        LiveId::from_str_num("area", self.0 as u64)
    }

    /// The dock's id for this node as a split.
    pub fn split_live_id(self) -> LiveId {
        LiveId::from_str_num("split", self.0 as u64)
    }
}

/// One editor's body: the editor and its number, 0 for the first (a repeatable editor's later
/// instances are bodies of their own).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, SerRon, DeRon)]
pub struct Instance {
    pub editor: Editor,
    pub n: u32,
}

impl Instance {
    pub fn first(editor: Editor) -> Instance {
        Instance { editor, n: 0 }
    }

    /// Its dock tab: the editor's own tab for the first instance.
    pub fn tab(self) -> LiveId {
        if self.n == 0 { self.editor.tab() } else { LiveId::from_num(self.editor.tab().0, self.n as u64) }
    }
}

/// The side of a node a new area takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Up,
    Down,
}

impl Side {
    fn axis(self) -> SplitterAxis {
        match self {
            Side::Left | Side::Right => SplitterAxis::Horizontal,
            Side::Up | Side::Down => SplitterAxis::Vertical,
        }
    }

    /// The new area is the split's first part (left or above).
    fn first(self) -> bool {
        matches!(self, Side::Left | Side::Up)
    }

    /// The sash of a side panel added on this side of several areas: a column as wide as the
    /// workspaces' side columns, or a row, pinned on the new panel's side.
    fn panel(self) -> SplitterAlign {
        match self {
            Side::Left => SplitterAlign::FromA(PANEL_W),
            Side::Right => SplitterAlign::FromB(PANEL_W),
            Side::Up => SplitterAlign::FromA(PANEL_H),
            Side::Down => SplitterAlign::FromB(PANEL_H),
        }
    }
}

/// A side panel's width and a bottom row's height, when a drop adds one beside several areas.
const PANEL_W: f64 = 320.0;
const PANEL_H: f64 = 240.0;

/// Where a moved tab or area lands on its target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropAt {
    /// A new area on that side of the target node: an area, or a split (beside all it holds).
    Side(Side),
    /// The target area's stack, last and in front (its middle, or its tab bar's empty end).
    Center,
    /// The target area's stack before its tab `k`, in front.
    Before(usize),
}

/// What a drag carries: one tab (its tab), or a whole area with its stack (its nub).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moving {
    Tab(Instance),
    Area(AreaId),
}

/// A change to a workspace's areas. Each has a gesture in the UI that makes it.
#[derive(Clone, Copy, Debug)]
pub enum WorkspaceOp {
    /// `editor` in front of `area` (an editor row of the area's menu): one the area holds comes
    /// forward; otherwise it takes the front tab's place, a repeatable editor as its lowest
    /// instance no area holds, any other trading places with the front tab when another area
    /// holds it.
    SetEditor { area: AreaId, editor: Editor },
    /// The two areas trade their stacks (the area menu's Swap with).
    Swap { a: AreaId, b: AreaId },
    /// A new area on `side` of node `area` holding `editor`, the split's sash at `align` (the area
    /// menu's Split right and Split down, the code toggle).
    Split { area: AreaId, side: Side, editor: Editor, align: SplitterAlign },
    /// The area goes and the part beside it takes its room (the area menu's Close area).
    Close { area: AreaId },
    /// One tab of `area` goes; the last one closes the area (the area menu's Close tab).
    CloseTab { area: AreaId, instance: Instance },
    /// A split's sash at `align` (a sash dragged; the dock reports it).
    Resize { split: AreaId, align: SplitterAlign },
    /// `instance` in front of its stack (a tab pressed; the dock reports it).
    Select { area: AreaId, instance: Instance },
    /// `editor` on screen (a Show or Reveal command): in front where an area holds it. When none
    /// does, it joins, in front, the stack of the area holding its nearest neighbour in its
    /// default layout ([`crate::workspaces::neighbours`]), else of `from` (the area the command
    /// came from); never an area a view fronts ([`Editor::keeps_corner`]), which it would cover.
    /// With neither, a panel opens as a side column ([`PANEL_W`] wide) at the right of `from` when
    /// a view fronts it, else of the workspace's first view; a view, or a workspace with no view,
    /// splits the last area (the bottom or right one) down, the new half holding it.
    Reveal { editor: Editor, from: Option<AreaId> },
    /// A drag dropped: `what` leaves its place (an area it empties closes) and lands `at` node
    /// `to`. A tab no area holds (a mode's tab the shell shows) lands as a new tab.
    Move { what: Moving, to: AreaId, at: DropAt },
    /// `area` alone fills the workspace, or (none) every area shows again (the area menu's
    /// Maximize and Restore). The workspace keeps it ([`crate::workspaces::Workspaces::edit`]);
    /// the tree is not changed by it.
    Maximize { area: Option<AreaId> },
}

/// Why an op changed nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpError {
    /// No area (or no node) has this id, or the area does not hold what the op names.
    NoArea(AreaId),
    /// No split has this id.
    NoSplit(AreaId),
    /// The last area cannot close.
    LastArea,
    /// A non-repeatable editor that this area holds already: Swap or a drag moves it.
    OnScreen(AreaId),
    /// It is so already.
    Unchanged,
}

/// Whether two sash positions are the same.
pub fn same_align(a: SplitterAlign, b: SplitterAlign) -> bool {
    match (a, b) {
        (SplitterAlign::FromA(x), SplitterAlign::FromA(y))
        | (SplitterAlign::FromB(x), SplitterAlign::FromB(y))
        | (SplitterAlign::Weighted(x), SplitterAlign::Weighted(y)) => (x - y).abs() < 1e-6,
        _ => false,
    }
}

/// A workspace's arrangement: an area holding a stack of editor instances (`selected` in front),
/// or two parts split.
#[derive(Clone, Debug, SerRon, DeRon)]
pub enum AreaTree {
    Area { id: AreaId, tabs: Vec<Instance>, selected: usize },
    Split { id: AreaId, axis: SplitterAxis, align: SplitterAlign, a: Box<AreaTree>, b: Box<AreaTree> },
}

/// Two trees are the same arrangement: the same areas, stacks and splits, sashes at the same
/// places ([`same_align`]; the dock's alignments have no equality of their own).
impl PartialEq for AreaTree {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (AreaTree::Area { id, tabs, selected }, AreaTree::Area { id: id2, tabs: tabs2, selected: selected2 }) =>
                id == id2 && tabs == tabs2 && selected == selected2,
            (AreaTree::Split { id, axis, align, a, b }, AreaTree::Split { id: id2, axis: axis2, align: align2, a: a2, b: b2 }) =>
                id == id2 && axis == axis2 && same_align(*align, *align2) && a == a2 && b == b2,
            _ => false,
        }
    }
}

impl AreaTree {
    /// An area holding `editor`, to build a tree with; [`AreaTree::numbered`] gives the ids.
    pub fn area(editor: Editor) -> AreaTree {
        AreaTree::Area { id: AreaId(0), tabs: vec![Instance::first(editor)], selected: 0 }
    }

    /// An area holding `editors` as tabs, `selected` in front.
    pub fn stack(editors: &[Editor], selected: usize) -> AreaTree {
        AreaTree::Area { id: AreaId(0), tabs: editors.iter().map(|e| Instance::first(*e)).collect(), selected }
    }

    /// `a` left of `b`, `share` of the width to `a`.
    pub fn beside(a: AreaTree, b: AreaTree, share: f64) -> AreaTree {
        AreaTree::split(SplitterAxis::Horizontal, SplitterAlign::Weighted(share), a, b)
    }

    /// `a` over `b`, `share` of the height to `a`.
    pub fn over(a: AreaTree, b: AreaTree, share: f64) -> AreaTree {
        AreaTree::split(SplitterAxis::Vertical, SplitterAlign::Weighted(share), a, b)
    }

    /// `a` and `b` split along `axis`, the sash at `align`.
    pub fn split(axis: SplitterAxis, align: SplitterAlign, a: AreaTree, b: AreaTree) -> AreaTree {
        AreaTree::Split { id: AreaId(0), axis, align, a: Box::new(a), b: Box::new(b) }
    }

    /// Numbers nodes in order (a split before its parts, the first part first). A repeatable
    /// editor keeps each tab's instance number unless an earlier tab of the same editor
    /// already holds it. A duplicate takes that editor's lowest free number. Other editors use 0.
    /// O(tabs²) worst case per tree, on loads and edits only, never per frame.
    pub fn numbered(mut self) -> AreaTree {
        fn walk(tree: &mut AreaTree, next: &mut u32, seen: &mut HashMap<Editor, (Vec<u32>, u32)>) {
            match tree {
                AreaTree::Area { id, tabs, selected } => {
                    *id = AreaId(*next);
                    *next += 1;
                    *selected = (*selected).min(tabs.len().saturating_sub(1));
                    for instance in tabs {
                        if !instance.editor.repeatable() {
                            instance.n = 0;
                            continue;
                        }
                        let (used, candidate) = seen.entry(instance.editor).or_default();
                        if used.contains(&instance.n) {
                            while used.contains(candidate) {
                                *candidate += 1;
                            }
                            instance.n = *candidate;
                        }
                        used.push(instance.n);
                    }
                }
                AreaTree::Split { id, a, b, .. } => {
                    *id = AreaId(*next);
                    *next += 1;
                    walk(a, next, seen);
                    walk(b, next, seen);
                }
            }
        }
        walk(&mut self, &mut 0, &mut HashMap::new());
        self
    }

    /// Every area holds at least one editor (a tree read from a file may not).
    pub fn whole(&self) -> bool {
        self.stacks().iter().all(|(_, tabs, _)| !tabs.is_empty())
    }

    pub fn id(&self) -> AreaId {
        match self {
            AreaTree::Area { id, .. } | AreaTree::Split { id, .. } => *id,
        }
    }

    /// Every area and its stack with the front tab's index, in order: left before right, top
    /// before bottom.
    pub fn stacks(&self) -> Vec<(AreaId, Vec<Instance>, usize)> {
        let mut out = Vec::new();
        self.visit(&mut |node| {
            if let AreaTree::Area { id, tabs, selected } = node {
                out.push((*id, tabs.clone(), *selected));
            }
        });
        out
    }

    /// Every area and its front instance, in order.
    pub fn areas(&self) -> Vec<(AreaId, Instance)> {
        self.stacks().into_iter().filter_map(|(id, tabs, selected)| tabs.get(selected).map(|i| (id, *i))).collect()
    }

    /// Every instance an area holds, front or not, with its area.
    pub fn instances(&self) -> Vec<(AreaId, Instance)> {
        self.stacks().into_iter().flat_map(|(id, tabs, _)| tabs.into_iter().map(move |i| (id, i))).collect()
    }

    /// Some area holds an instance `f` accepts, front or not. O(tabs), no allocation (asked many
    /// times a frame through the state's `shows`).
    pub fn any_instance(&self, f: &impl Fn(&Instance) -> bool) -> bool {
        match self {
            AreaTree::Area { tabs, .. } => tabs.iter().any(f),
            AreaTree::Split { a, b, .. } => a.any_instance(f) || b.any_instance(f),
        }
    }

    /// The tree has a node (area or split) `id`.
    pub fn contains(&self, id: AreaId) -> bool {
        self.node(id).is_some()
    }

    /// Split `split`'s axis and its two parts.
    pub fn parts(&self, split: AreaId) -> Option<(SplitterAxis, AreaId, AreaId)> {
        match self.node(split)? {
            AreaTree::Split { axis, a, b, .. } => Some((*axis, a.id(), b.id())),
            AreaTree::Area { .. } => None,
        }
    }

    fn visit(&self, f: &mut impl FnMut(&AreaTree)) {
        f(self);
        if let AreaTree::Split { a, b, .. } = self {
            a.visit(f);
            b.visit(f);
        }
    }

    fn node(&self, want: AreaId) -> Option<&AreaTree> {
        if self.id() == want {
            return Some(self);
        }
        match self {
            AreaTree::Split { a, b, .. } => a.node(want).or_else(|| b.node(want)),
            AreaTree::Area { .. } => None,
        }
    }

    fn node_mut(&mut self, want: AreaId) -> Option<&mut AreaTree> {
        if self.id() == want {
            return Some(self);
        }
        match self {
            AreaTree::Split { a, b, .. } => a.node_mut(want).or_else(|| b.node_mut(want)),
            AreaTree::Area { .. } => None,
        }
    }

    /// `area`'s stack and front index.
    pub fn stack_of(&self, area: AreaId) -> Option<(Vec<Instance>, usize)> {
        match self.node(area)? {
            AreaTree::Area { tabs, selected, .. } => Some((tabs.clone(), *selected)),
            AreaTree::Split { .. } => None,
        }
    }

    pub(crate) fn stack_mut(&mut self, area: AreaId) -> Option<(&mut Vec<Instance>, &mut usize)> {
        match self.node_mut(area)? {
            AreaTree::Area { tabs, selected, .. } => Some((tabs, selected)),
            AreaTree::Split { .. } => None,
        }
    }

    /// What `area` shows in front.
    pub fn instance(&self, area: AreaId) -> Option<Instance> {
        self.stack_of(area).and_then(|(tabs, selected)| tabs.get(selected).copied())
    }

    /// The area holding the tab `tab`, front or not.
    pub fn area_of_tab(&self, tab: LiveId) -> Option<AreaId> {
        self.instances().into_iter().find(|(_, i)| i.tab() == tab).map(|(id, _)| id)
    }

    /// The instance whose tab is `tab`.
    pub fn instance_of_tab(&self, tab: LiveId) -> Option<Instance> {
        self.instances().into_iter().find(|(_, i)| i.tab() == tab).map(|(_, i)| i)
    }

    /// The area holding `instance`.
    pub fn holding(&self, instance: Instance) -> Option<AreaId> {
        self.instances().into_iter().find(|(_, i)| *i == instance).map(|(id, _)| id)
    }

    /// The first area holding an instance of `editor`, front or not.
    pub fn showing(&self, editor: Editor) -> Option<AreaId> {
        self.instances().into_iter().find(|(_, i)| i.editor == editor).map(|(id, _)| id)
    }

    /// The split the dock knows as `live` (the top one is the dock's root, [`present`]).
    pub fn split_of_live_id(&self, live: LiveId) -> Option<AreaId> {
        if let (AreaTree::Split { id, .. }, true) = (self, live == live_id!(root)) {
            return Some(*id);
        }
        let mut found = None;
        self.visit(&mut |node| {
            if let AreaTree::Split { id, .. } = node {
                if id.split_live_id() == live {
                    found = Some(*id);
                }
            }
        });
        found
    }

    /// The area the dock knows as `live` (its tab set; the dock's root when it is the only area).
    pub fn area_of_live_id(&self, live: LiveId) -> Option<AreaId> {
        if let (AreaTree::Area { id, .. }, true) = (self, live == live_id!(root)) {
            return Some(*id);
        }
        self.stacks().into_iter().map(|(id, ..)| id).find(|id| id.area_live_id() == live)
    }

    /// The split `area` is a part of: its id, axis and sash, and whether `area` is its second
    /// part (right or below). None for the top area. O(nodes).
    pub fn parent_split(&self, area: AreaId) -> Option<(AreaId, SplitterAxis, SplitterAlign, bool)> {
        let mut found = None;
        self.visit(&mut |node| {
            if let AreaTree::Split { id, axis, align, a, b } = node {
                if a.id() == area || b.id() == area {
                    found = Some((*id, *axis, *align, b.id() == area));
                }
            }
        });
        found
    }

    /// The editors near `editor`, nearest first: the others of its stack, then those of each
    /// split around it, inner to outer, each part in order. None when no area holds it. O(tabs).
    pub fn nearby(&self, editor: Editor) -> Option<Vec<Editor>> {
        let all = |tree: &AreaTree| tree.instances().into_iter().map(|(_, i)| i.editor).collect::<Vec<_>>();
        match self {
            AreaTree::Area { tabs, .. } => tabs.iter().any(|i| i.editor == editor)
                .then(|| tabs.iter().map(|i| i.editor).filter(|e| *e != editor).collect()),
            AreaTree::Split { a, b, .. } => {
                if let Some(mut near) = a.nearby(editor) {
                    near.extend(all(b));
                    return Some(near);
                }
                let mut near = b.nearby(editor)?;
                near.extend(all(a));
                Some(near)
            }
        }
    }

    /// The area last in order: the bottom or right one.
    pub fn last_area(&self) -> AreaId {
        match self {
            AreaTree::Area { id, .. } => *id,
            AreaTree::Split { b, .. } => b.last_area(),
        }
    }

    fn next_id(&self) -> AreaId {
        let mut max = 0;
        self.visit(&mut |node| max = max.max(node.id().0));
        AreaId(max + 1)
    }

    /// The lowest instance of `editor` no area holds and `taken` does not name.
    fn free_instance(&self, editor: Editor, taken: &[Instance]) -> Instance {
        let held = self.instances();
        let n = (0..).find(|n| {
            let i = Instance { editor, n: *n };
            !held.iter().any(|(_, h)| *h == i) && !taken.contains(&i)
        }).unwrap_or(0);
        Instance { editor, n }
    }

    /// Each split's sash, by its id.
    pub fn sizes(&self) -> Vec<(AreaId, SplitterAlign)> {
        let mut out = Vec::new();
        self.visit(&mut |node| {
            if let AreaTree::Split { id, align, .. } = node {
                out.push((*id, *align));
            }
        });
        out
    }

    /// The sashes of this tree that differ from `base`'s, for the splits both have between the
    /// same two parts (a split an edit made or changed is not one of them).
    pub fn sizes_off(&self, base: &AreaTree) -> Vec<(AreaId, SplitterAlign)> {
        let parts = |tree: &AreaTree| {
            let mut out = Vec::new();
            tree.visit(&mut |node| {
                if let AreaTree::Split { id, align, a, b, .. } = node {
                    out.push((*id, *align, a.id(), b.id()));
                }
            });
            out
        };
        let base = parts(base);
        parts(self).into_iter().filter_map(|(id, align, a, b)| {
            base.iter().find(|(bid, _, ba, bb)| *bid == id && *ba == a && *bb == b)
                .filter(|(_, was, ..)| !same_align(*was, align))
                .map(|_| (id, align))
        }).collect()
    }

    /// Puts the sashes `sizes` keeps on the splits that have them.
    pub fn set_sizes(&mut self, sizes: &[(AreaId, SplitterAlign)]) {
        for (id, size) in sizes {
            if let Some(AreaTree::Split { align, .. }) = self.node_mut(*id) {
                *align = *size;
            }
        }
    }

    /// The split holding `child`, and whether as its first part.
    fn parent_of(&self, child: AreaId) -> Option<(AreaId, bool)> {
        match self {
            AreaTree::Area { .. } => None,
            AreaTree::Split { id, a, b, .. } => {
                if a.id() == child {
                    Some((*id, true))
                } else if b.id() == child {
                    Some((*id, false))
                } else {
                    a.parent_of(child).or_else(|| b.parent_of(child))
                }
            }
        }
    }

    /// Takes node `area` out, the part beside it taking its split's place: the split that went
    /// and the node standing where it stood.
    fn remove_node(&mut self, area: AreaId) -> Result<(AreaId, AreaId), OpError> {
        let (split, first) = self.parent_of(area).ok_or(OpError::LastArea)?;
        let mut sibling = None;
        if let Some(node) = self.node_mut(split) {
            let placeholder = AreaTree::Area { id: split, tabs: Vec::new(), selected: 0 };
            if let AreaTree::Split { a, b, .. } = std::mem::replace(node, placeholder) {
                let keep = if first { *b } else { *a };
                sibling = Some(keep.id());
                *node = keep;
            }
        }
        sibling.map(|s| (split, s)).ok_or(OpError::NoSplit(split))
    }

    /// Node `at` split, a new area on `side` holding `tabs` (the first in front), the sash at
    /// `align`; the new area's id.
    fn split_with(&mut self, at: AreaId, side: Side, tabs: Vec<Instance>, align: SplitterAlign) -> AreaId {
        let new_area = self.next_id();
        let split = AreaId(new_area.0 + 1);
        if let Some(node) = self.node_mut(at) {
            let new = AreaTree::Area { id: new_area, tabs, selected: 0 };
            let old = std::mem::replace(node, new.clone());
            let (a, b) = if side.first() { (new, old) } else { (old, new) };
            *node = AreaTree::Split { id: split, axis: side.axis(), align, a: Box::new(a), b: Box::new(b) };
        }
        new_area
    }

    /// Each split's sash in a window `room` wide while it is narrow ([`Present::narrow`]): the
    /// sashes a narrow load would have, for a window resized while narrow. O(splits).
    pub fn narrow_sizes(&self, room: f64) -> Vec<(AreaId, SplitterAlign)> {
        fn walk(tree: &AreaTree, room: Option<f64>, out: &mut Vec<(AreaId, SplitterAlign)>) {
            if let AreaTree::Split { id, axis, align, a, b } = tree {
                let (align, room_a, room_b) = narrowed(*axis, *align, room);
                out.push((*id, align));
                walk(a, room_a, out);
                walk(b, room_b, out);
            }
        }
        let mut out = Vec::new();
        walk(self, Some(room), &mut out);
        out
    }

    /// Applies `op`: the tree changed, or why it did not.
    pub fn apply(&mut self, op: WorkspaceOp) -> Result<(), OpError> {
        self.apply_taking(op, &[])
    }

    /// [`Self::apply`], a new instance (a split's, an editor set or revealed) never one `taken`
    /// names: the Plate views other workspaces hold, so a new Plate view is a fresh one.
    pub fn apply_taking(&mut self, op: WorkspaceOp, taken: &[Instance]) -> Result<(), OpError> {
        match op {
            WorkspaceOp::SetEditor { area, editor } => {
                let (tabs, selected) = self.stack_of(area).ok_or(OpError::NoArea(area))?;
                let front = tabs[selected];
                if front.editor == editor {
                    return Err(OpError::Unchanged);
                }
                if let Some(k) = tabs.iter().position(|i| i.editor == editor) {
                    if let Some((_, selected)) = self.stack_mut(area) {
                        *selected = k;
                    }
                    return Ok(());
                }
                let held = (!editor.repeatable()).then(|| self.instances().into_iter().find(|(_, i)| i.editor == editor)).flatten();
                if let Some((other, theirs)) = held {
                    // the editor comes here, this area's front tab goes where it was
                    if let Some((tabs, _)) = self.stack_mut(other) {
                        if let Some(slot) = tabs.iter_mut().find(|i| **i == theirs) {
                            *slot = front;
                        }
                    }
                    if let Some((tabs, selected)) = self.stack_mut(area) {
                        tabs[*selected] = theirs;
                    }
                    return Ok(());
                }
                let instance = self.free_instance(editor, taken);
                if let Some((tabs, selected)) = self.stack_mut(area) {
                    tabs[*selected] = instance;
                }
                Ok(())
            }
            WorkspaceOp::Swap { a, b } => {
                let sa = self.stack_of(a).ok_or(OpError::NoArea(a))?;
                let sb = self.stack_of(b).ok_or(OpError::NoArea(b))?;
                if a == b {
                    return Err(OpError::Unchanged);
                }
                if let Some((tabs, selected)) = self.stack_mut(a) {
                    (*tabs, *selected) = sb;
                }
                if let Some((tabs, selected)) = self.stack_mut(b) {
                    (*tabs, *selected) = sa;
                }
                Ok(())
            }
            WorkspaceOp::Split { area, side, editor, align } => {
                self.node(area).ok_or(OpError::NoArea(area))?;
                if !editor.repeatable() {
                    if let Some(other) = self.showing(editor) {
                        return Err(OpError::OnScreen(other));
                    }
                }
                let instance = self.free_instance(editor, taken);
                self.split_with(area, side, vec![instance], align);
                Ok(())
            }
            WorkspaceOp::Close { area } => {
                self.stack_of(area).ok_or(OpError::NoArea(area))?;
                self.remove_node(area).map(|_| ())
            }
            WorkspaceOp::CloseTab { area, instance } => {
                let (tabs, _) = self.stack_of(area).ok_or(OpError::NoArea(area))?;
                let k = tabs.iter().position(|i| *i == instance).ok_or(OpError::NoArea(area))?;
                if tabs.len() == 1 {
                    return self.apply(WorkspaceOp::Close { area });
                }
                if let Some((tabs, selected)) = self.stack_mut(area) {
                    tabs.remove(k);
                    if k < *selected {
                        *selected -= 1;
                    }
                    *selected = (*selected).min(tabs.len() - 1);
                }
                Ok(())
            }
            WorkspaceOp::Resize { split, align } => match self.node_mut(split) {
                Some(AreaTree::Split { align: at, .. }) => {
                    *at = align;
                    Ok(())
                }
                _ => Err(OpError::NoSplit(split)),
            },
            WorkspaceOp::Select { area, instance } => {
                let (tabs, selected) = self.stack_of(area).ok_or(OpError::NoArea(area))?;
                let k = tabs.iter().position(|i| *i == instance).ok_or(OpError::NoArea(area))?;
                if k == selected {
                    return Err(OpError::Unchanged);
                }
                if let Some((_, selected)) = self.stack_mut(area) {
                    *selected = k;
                }
                Ok(())
            }
            WorkspaceOp::Reveal { editor, from } => {
                if let Some((area, instance)) = self.instances().into_iter().find(|(_, i)| i.editor == editor) {
                    return self.apply(WorkspaceOp::Select { area, instance });
                }
                let instance = self.free_instance(editor, taken);
                let open = |area: AreaId| self.stack_of(area).is_some_and(|(tabs, k)| tabs.get(k).is_some_and(|i| !i.editor.keeps_corner()));
                let join = crate::workspaces::neighbours(editor).into_iter().filter_map(|e| self.showing(e)).chain(from).find(|a| open(*a));
                // with no stack to join, a panel opens as a side column at the right of the view
                // it was asked from, else of the workspace's first view, as wide as the side
                // columns; a view (or a workspace without one) splits the last area down
                let view = from.filter(|a| self.stack_of(*a).is_some() && !open(*a))
                    .or_else(|| self.instances().into_iter().find(|(_, i)| i.editor.keeps_corner()).map(|(a, _)| a))
                    .filter(|_| !editor.keeps_corner());
                match join.and_then(|area| self.stack_mut(area)) {
                    Some((tabs, selected)) => {
                        tabs.push(instance);
                        *selected = tabs.len() - 1;
                    }
                    None => {
                        let (at, side, align) = match view {
                            Some(area) => (area, Side::Right, Side::Right.panel()),
                            None => (self.last_area(), Side::Down, SplitterAlign::Weighted(0.5)),
                        };
                        self.split_with(at, side, vec![instance], align);
                    }
                }
                Ok(())
            }
            WorkspaceOp::Move { what, to, at } => self.move_to(what, to, at),
            // the workspace's, not the tree's
            WorkspaceOp::Maximize { .. } => Err(OpError::Unchanged),
        }
    }

    /// [`WorkspaceOp::Move`]: checked whole before anything changes.
    fn move_to(&mut self, what: Moving, mut to: AreaId, mut at: DropAt) -> Result<(), OpError> {
        self.node(to).ok_or(OpError::NoArea(to))?;
        if matches!(at, DropAt::Center | DropAt::Before(_)) && self.stack_of(to).is_none() {
            return Err(OpError::NoArea(to));
        }
        // what moves, the one of it in front, and the area it leaves
        let (from, moved, front) = match what {
            Moving::Tab(instance) => {
                if !instance.editor.repeatable() && self.holding(instance).is_none() {
                    if let Some(other) = self.showing(instance.editor) {
                        return Err(OpError::OnScreen(other));
                    }
                }
                (self.holding(instance), vec![instance], instance)
            }
            Moving::Area(area) => {
                let (tabs, selected) = self.stack_of(area).ok_or(OpError::NoArea(area))?;
                let front = tabs[selected];
                (Some(area), tabs, front)
            }
        };
        if let Some(from) = from {
            let (tabs, selected) = self.stack_of(from).ok_or(OpError::NoArea(from))?;
            let whole = matches!(what, Moving::Area(_)) || tabs.len() == 1;
            if from == to && (whole || at == DropAt::Center) {
                return Err(OpError::Unchanged);
            }
            if whole {
                let (split, sibling) = self.remove_node(from)?;
                if to == split {
                    to = sibling;
                }
            } else {
                let k = tabs.iter().position(|i| *i == front).ok_or(OpError::NoArea(from))?;
                if let DropAt::Before(j) = at {
                    if from == to && (j == k || j == k + 1) {
                        return Err(OpError::Unchanged);
                    }
                    if from == to && k < j {
                        at = DropAt::Before(j - 1);
                    }
                }
                if let Some((tabs, sel)) = self.stack_mut(from) {
                    tabs.remove(k);
                    *sel = (if k < selected { selected - 1 } else { selected }).min(tabs.len() - 1);
                }
            }
        }
        let front_at = moved.iter().position(|i| *i == front).unwrap_or(0);
        match at {
            DropAt::Side(side) => {
                // an area's edge halves it; the edge of several (the whole, a split) adds a panel
                let align = if self.stack_of(to).is_some() { SplitterAlign::Weighted(0.5) } else { side.panel() };
                let area = self.split_with(to, side, moved, align);
                if let Some((_, selected)) = self.stack_mut(area) {
                    *selected = front_at;
                }
            }
            DropAt::Center | DropAt::Before(_) => {
                let (tabs, selected) = self.stack_mut(to).ok_or(OpError::NoArea(to))?;
                let k = match at {
                    DropAt::Before(j) => j.min(tabs.len()),
                    _ => tabs.len(),
                };
                let n = moved.len();
                tabs.splice(k..k, moved);
                *selected = k + front_at.min(n - 1);
            }
        }
        Ok(())
    }

    /// The tree's tabs, tab sets and splits into `map` as `how` shows them ([`present`]); the
    /// root's dock id. `room`: the width this node has, when the window is narrow.
    fn put(&self, map: &mut DockMap, how: &Present, extra: &[(Editor, Editor, bool)], room: Option<f64>) -> LiveId {
        match self {
            AreaTree::Area { id, tabs, selected } => {
                let mut out: Vec<LiveId> = Vec::new();
                let mut alone = None;
                let mut front = None;
                for (k, instance) in tabs.iter().enumerate() {
                    let tab = if how.ghost.contains(instance) {
                        if out.contains(&GHOST_TAB) {
                            // the drag's other tabs: one ghost stands for them all
                            if k == *selected {
                                front = out.iter().position(|t| *t == GHOST_TAB);
                            }
                            continue;
                        }
                        let name = how.ghost.first().map_or("", |i| i.editor.name());
                        map.insert(GHOST_TAB, DockItem::Tab { name: name.to_string(), template: live_id!(PermanentTab), kind: GHOST_KIND });
                        GHOST_TAB
                    } else {
                        map.insert(instance.tab(), tab_item(instance.editor));
                        instance.tab()
                    };
                    alone = Some(instance.editor);
                    if k == *selected {
                        front = Some(out.len());
                    }
                    out.push(tab);
                }
                for (beside, editor, forward) in extra {
                    if tabs.iter().any(|i| i.editor == *beside) {
                        map.insert(editor.tab(), tab_item(*editor));
                        out.push(editor.tab());
                        if *forward {
                            front = Some(out.len() - 1);
                        }
                    }
                }
                // one view without a toolbar of its own: no tab bar, only the nub
                let hide_tab_bar = out.len() == 1 && alone.is_some_and(|e| e.bare_alone());
                map.insert(id.area_live_id(), DockItem::Tabs { tabs: out, selected: front.unwrap_or(0), closable: false, hide_tab_bar });
                id.area_live_id()
            }
            AreaTree::Split { id, axis, align, a, b } => {
                let (align, room_a, room_b) = narrowed(*axis, *align, room);
                let a = a.put(map, how, extra, room_a);
                let b = b.put(map, how, extra, room_b);
                map.insert(id.split_live_id(), DockItem::Splitter { axis: *axis, align, a, b });
                id.split_live_id()
            }
        }
    }
}

/// A split's sash while the window is narrow, `room` wide where the split stands (none: not
/// narrow, or a part above another), and the width each part then has: a pinned side column
/// keeps at most a third of its split.
fn narrowed(axis: SplitterAxis, align: SplitterAlign, room: Option<f64>) -> (SplitterAlign, Option<f64>, Option<f64>) {
    match (room, axis) {
        (Some(r), SplitterAxis::Horizontal) => {
            let align = match align {
                SplitterAlign::FromA(x) => SplitterAlign::FromA(x.min(r / 3.0)),
                SplitterAlign::FromB(x) => SplitterAlign::FromB(x.min(r / 3.0)),
                weighted => weighted,
            };
            let first = match align {
                SplitterAlign::FromA(x) => x,
                SplitterAlign::FromB(x) => r - x,
                SplitterAlign::Weighted(w) => r * w,
            };
            (align, Some(first), Some(r - first))
        }
        _ => (align, room, room),
    }
}

/// The dock's tab of an instance of `editor`.
fn tab_item(editor: Editor) -> DockItem {
    DockItem::Tab { name: editor.name().to_string(), template: live_id!(PermanentTab), kind: editor.kind() }
}

/// The dock's tab of a drag's ghost: what the drag carries, shown where its drop would put it.
pub const GHOST_TAB: LiveId = live_id!(fab_area_ghost);
/// The ghost's body: its template in the shell's `Dock`.
pub const GHOST_KIND: LiveId = live_id!(Ghost);

/// How a tree is shown in the dock: what the map adds to it or leaves out, never stored.
#[derive(Default)]
pub struct Present<'a> {
    /// Editors that come forward with a render mode while it is on, as `(beside, editor,
    /// forward)`: `editor` joins the stacks holding `beside` while no area holds it, in front
    /// when `forward` (the mode just came on).
    pub extra: &'a [(Editor, Editor, bool)],
    /// The instances a drag carries: one ghost tab where the drop would put them.
    pub ghost: &'a [Instance],
    /// The dock's width while the window is narrow: each pinned side column keeps at most a
    /// third of its split, in the map only (a wider window gets the stored sizes back).
    pub narrow: Option<f64>,
    /// This area alone fills the dock: the workspace's maximized area, or at a phone's width
    /// the area in use (the shell's choice).
    pub maximized: Option<AreaId>,
    /// An editor shown alone over the workspace (Edit › 3D Model Sources…): the dock holds its
    /// body only, without a tab bar, until Back. The tree is not changed by it.
    pub page: Option<Editor>,
}

/// `tree` as the dock's whole map, shown `how`: its top node is the dock's root.
pub fn present(tree: &AreaTree, how: &Present) -> DockMap {
    if let Some(editor) = how.page {
        let mut map = DockMap::new();
        map.insert(editor.tab(), tab_item(editor));
        map.insert(live_id!(root), DockItem::Tabs { tabs: vec![editor.tab()], selected: 0, closable: false, hide_tab_bar: true });
        return map;
    }
    let extra: Vec<(Editor, Editor, bool)> = how.extra.iter().copied().filter(|(_, editor, _)| tree.showing(*editor).is_none()).collect();
    let mut map = DockMap::new();
    let node = how.maximized.and_then(|area| tree.node(area)).filter(|node| matches!(node, AreaTree::Area { .. })).unwrap_or(tree);
    let top = node.put(&mut map, how, &extra, how.narrow);
    if let Some(item) = map.remove(&top) {
        map.insert(live_id!(root), item);
    }
    map
}

/// `tree` as the dock's whole map, as stored.
pub fn layout_map(tree: &AreaTree) -> DockMap {
    present(tree, &Present::default())
}

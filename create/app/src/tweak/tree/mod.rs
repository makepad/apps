//! The interactive tree belongs to this seat. A search has its own pages so clearing it is local.
pub(super) mod view;
use super::DesignPlace;
use super::focus::{FocusState, TargetPresence};
use super::{overlay::{seat_colour, who}, wire::TREE_PAGE, RequestId, SeatView, TreeQuery};
use makepad_strict_json::Value;
use makepad_town_core::fold::State;
use makepad_widgets::Vec4f;
use std::{collections::{BTreeMap, BTreeSet, HashMap, HashSet}, rc::Rc};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TreeNode {
    pub path: Rc<str>,
    pub name: String,
    pub ty: String,
    pub children: u32,
    pub visible: bool,
    pub matched: bool,
    pub rect: Option<super::Rect>,
}

#[derive(Debug, Default)]
pub(super) struct ParentPage {
    offset: u32,
    total: u32,
    nodes: Vec<TreeNode>,
}

impl ParentPage {
    fn end(&self) -> u32 { self.offset.saturating_add(self.nodes.len() as u32) }

    // The fresh slice wins at every position it covers, including a reveal window inside a page.
    fn merge(&mut self, fresh: Self) {
        if self.total != fresh.total || fresh.offset > self.end() || self.offset > fresh.end() {
            *self = fresh;
            return
        }
        let start = fresh.offset.saturating_sub(self.offset) as usize;
        let end = fresh.end().min(self.end()).saturating_sub(self.offset) as usize;
        self.nodes.splice(start..end, fresh.nodes);
        self.offset = self.offset.min(fresh.offset);
    }

    // The app visits matches in tree order. Later pages can repeat ancestors of earlier matches.
    fn merge_matches(&mut self, fresh: Self) {
        let mut positions: HashMap<Rc<str>, usize> = self.nodes.iter().enumerate()
            .map(|(i, n)| (n.path.clone(), i)).collect();
        for mut node in fresh.nodes {
            if let Some(&i) = positions.get(&node.path) {
                node.matched |= self.nodes[i].matched;
                self.nodes[i] = node;
            } else {
                positions.insert(node.path.clone(), self.nodes.len());
                self.nodes.push(node);
            }
        }
        self.total = self.nodes.len() as u32;
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct SeatDot { pub colour: Vec4f, pub label: String }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DropIndicator { Before, Inside, After }

impl DropIndicator {
    pub fn place(self) -> DesignPlace {
        match self { Self::Before => DesignPlace::Before, Self::Inside => DesignPlace::Inside, Self::After => DesignPlace::After }
    }
}

pub(super) fn is_self_or_descendant(source: &str, target: &str) -> bool {
    if source.is_empty() || target.is_empty() { return false }
    if source == target { return true }
    let sep = if source.contains('/') || target.contains('/') { '/' } else { '.' };
    let prefix = source.trim_end_matches(sep);
    if prefix.is_empty() {
        source == "/" && target.starts_with('/')
    } else {
        target.strip_prefix(prefix).is_some_and(|rest| rest.starts_with(sep))
    }
}

pub(super) fn drop_zone(source: &str, target: &str, target_ty: &str, rel_y: f64, row_height: f64) -> Option<DropIndicator> {
    if !rel_y.is_finite() || !row_height.is_finite() || row_height <= 0. || is_self_or_descendant(source, target) { return None }
    let y = (rel_y / row_height).clamp(0., 1.);
    let container = makepad_widgets::design_data::locate::is_container_type_name(target_ty);
    Some(if y < 0.25 || (!container && y < 0.5) { DropIndicator::Before }
        else if container && y < 0.75 { DropIndicator::Inside }
        else { DropIndicator::After })
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TreeLine {
    Node {
        path: Rc<str>, depth: u16, label: String, ty: String, dim: bool, has_children: bool,
        expanded: bool, selected: bool, even: bool, seats: Vec<SeatDot>,
        drop: Option<DropIndicator>,
    },
    Pager { parent: Rc<str>, depth: u16, offset: u32, label: String, is_search: bool },
    Info { depth: u16, text: String },
}

#[derive(Clone, Debug)]
pub(super) enum TreeAction {
    Select(String),
    Expand { path: String, offset: u32 },
    Collapse(String),
    Filter(String),
    LoadMoreMatches(u32),
    Refresh,
    Move { source: String, target: String, place: DesignPlace },
}

#[derive(Default)]
struct LatestSlot {
    in_flight: Option<RequestId>,
    sent: Option<String>,
    waiting: Option<String>,
}

impl LatestSlot {
    fn offer(&mut self, value: String) -> Option<String> {
        if self.in_flight.is_none() { return Some(value) }
        self.waiting = (self.sent.as_ref() != Some(&value)).then_some(value);
        None
    }

    fn mark_sent(&mut self, req: RequestId, value: &str) {
        self.in_flight = Some(req);
        self.sent = Some(value.to_owned());
    }

    /// Keep the request ID so a post-Escape reply or refusal is still ours. Retyping the
    /// same query now starts at offset zero, even if the cancelled request was for page two.
    fn cancel(&mut self) {
        self.sent = None;
        self.waiting = None;
    }

    fn finish(&mut self, req: RequestId) -> Option<(bool, Option<String>)> {
        if self.in_flight != Some(req) { return None }
        self.in_flight = None;
        let current = self.sent.take().is_some() && self.waiting.is_none();
        Some((current, self.waiting.take()))
    }
}

#[derive(Default)]
pub(super) struct TreeState {
    pub focus: FocusState,
    pub loaded: bool,
    refreshing: bool,
    pub total_nodes: u32,
    pages: BTreeMap<String, ParentPage>,
    nodes: HashMap<String, TreeNode>,
    parent_of: HashMap<String, String>,
    expanded: BTreeSet<String>,
    loading: BTreeSet<String>,
    pub filter: String,
    filter_pages: BTreeMap<String, ParentPage>,
    filter_nodes: HashMap<Rc<str>, TreeNode>,
    filter_result: String,
    filter_total_matches: u32,
    filter_loaded_matches: u32,
    filter_truncated: bool,
    search_slot: LatestSlot,
    reveal_slot: LatestSlot,
    focus_reveal: Option<RequestId>,
    inflight_expand: HashMap<RequestId, String>,
    pub inflight_root: Option<RequestId>,
    retired: HashSet<RequestId>,
    pub scroll_to: Option<Rc<str>>,
    pub status: String,
    seat_dots: HashMap<String, Vec<SeatDot>>,
    row_indices: HashMap<Rc<str>, usize>,
}

fn search(query: String, offset: u32) -> TreeQuery { TreeQuery::Search { query, offset, limit: TREE_PAGE } }
pub(super) fn reveal(path: String) -> TreeQuery { TreeQuery::Reveal { path, limit: TREE_PAGE } }

fn count(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0).min(u32::MAX.into()) as u32
}

// These blocks have passed wire::validate_tree or the local session's valid_tree check.
fn read_pages(v: &Value) -> impl Iterator<Item = (String, ParentPage)> + '_ {
    let window = v.get("window").and_then(Value::as_u64).and_then(|w| usize::try_from(w).ok());
    let rect = move |n: &Value| {
        let [x, y, w, h] = n.get("rect")?.as_arr()? else { return None };
        let num = |v: &Value| match v { Value::Int(i) => Some(*i as f64), Value::F64(f) => Some(*f), _ => None };
        Some(super::Rect { window: window?, x: num(x)?, y: num(y)?, w: num(w)?, h: num(h)? })
    };
    v.get("parents").and_then(Value::as_arr).into_iter().flatten().filter_map(move |p| {
        let parent = p.get("path")?.as_str()?.to_owned();
        let nodes = p.get("nodes")?.as_arr()?.iter().filter_map(|n| Some(TreeNode {
            path: n.get("path")?.as_str()?.into(), name: n.get("name")?.as_str()?.into(),
            ty: n.get("ty")?.as_str()?.into(), children: count(n, "children"),
            visible: n.get("visible")?.as_bool()?, rect: rect(n), matched: n.get("matched").and_then(Value::as_bool).unwrap_or(false),
        })).collect();
        Some((parent, ParentPage { offset: count(p, "offset"), total: count(p, "total"), nodes }))
    })
}

impl TreeState {
    pub fn root() -> TreeQuery { TreeQuery::Children { path: None, depth: 3, offset: 0, limit: TREE_PAGE } }

    pub fn clear_for_refresh(&mut self) -> TreeQuery {
        self.retired.extend(self.inflight_root);
        self.retired.extend(self.inflight_expand.keys().copied());
        for slot in [&self.search_slot, &self.reveal_slot] {
            self.retired.extend(slot.in_flight);
        }
        *self = Self { refreshing: self.loaded || self.refreshing, focus: std::mem::take(&mut self.focus),
            expanded: std::mem::take(&mut self.expanded), retired: std::mem::take(&mut self.retired), seat_dots: std::mem::take(&mut self.seat_dots),
            ..Default::default() };
        Self::root()
    }

    pub fn offer_search(&mut self, query: String) -> Option<TreeQuery> {
        let clean: String = query.chars().filter(|c| !c.is_control()).collect();
        let clean = clean.trim();
        if clean == self.filter { return None }
        self.filter = clean.to_owned();
        self.status.clear();
        if clean.is_empty() {
            self.search_slot.cancel();
            self.filter_result.clear();
            self.filter_pages.clear();
            self.filter_nodes.clear();
            self.filter_truncated = false;
            return None
        }
        self.search_slot.offer(clean.to_owned()).map(|q| search(q, 0))
    }

    pub fn more_matches(&self, offset: u32) -> Option<TreeQuery> {
        (self.search_slot.in_flight.is_none() && self.filter == self.filter_result && self.filter_truncated
            && offset == self.filter_loaded_matches).then(|| search(self.filter.clone(), offset))
    }

    pub fn cancel_reveal(&mut self) {
        // Selection-follow and isolation share the reveal slot. Leaving a tab or clearing a
        // search must still let the isolated target's outstanding geometry arrive.
        if !self.focus.isolated_path.as_ref().is_some_and(|p| {
            self.reveal_slot.sent.as_ref() == Some(p) || self.reveal_slot.waiting.as_ref() == Some(p)
        }) { self.reveal_slot.cancel(); }
        self.scroll_to = None;
    }

    pub fn offer_reveal(&mut self, path: &str) -> Option<TreeQuery> {
        self.scroll_to = None;
        self.reveal_slot.offer(path.into()).map(reveal)
    }

    pub fn request_expand(&mut self, path: &str, offset: u32) -> Option<TreeQuery> {
        if self.focus.isolated_path.as_deref() != Some(path) { self.expanded.insert(path.into()); }
        if self.loading.contains(path) || self.pages.get(path).is_some_and(|p| {
            p.total == 0 || (p.offset <= offset && offset < p.end())
        }) { return None }
        Some(TreeQuery::Children { path: (!path.is_empty()).then(|| path.into()), depth: 1, offset, limit: TREE_PAGE })
    }

    pub fn collapse(&mut self, path: &str) {
        if self.focus.isolated_path.as_deref() != Some(path) { self.expanded.remove(path); }
    }

    /// Complete the subtree with ordinary tree reads, preserving the shared selection.
    pub fn focus_query(&mut self) -> Option<TreeQuery> {
        let path = self.focus.isolated_path.clone()?;
        if self.inflight_root.is_some() { return None }
        let Some(node) = self.nodes.get(&path) else {
            if self.reveal_slot.sent.as_ref() == Some(&path) || self.reveal_slot.waiting.as_ref() == Some(&path) {
                return None
            }
            return self.offer_reveal(&path)
        };
        if node.children > 0 && !self.pages.contains_key(&path) {
            return self.request_expand(&path, 0)
        }
        None
    }

    fn sight(focus: &mut FocusState, node: &TreeNode) {
        if focus.isolated_path.as_deref() == Some(&node.path) {
            if let Some(r) = node.rect.filter(|_| node.visible) { focus.update_rect(r); }
            else { focus.settle(TargetPresence::Missing); }
        }
    }

    /// A shallow page's omission is inconclusive until a reveal of the target finishes.
    fn target_presence(sighted: bool, revealed: bool) -> TargetPresence {
        if sighted { TargetPresence::Present }
        else if revealed { TargetPresence::Missing }
        else { TargetPresence::Pending }
    }

    pub fn toggle_isolate(&mut self, path: &str, rect: Option<super::Rect>) -> Option<TreeQuery> {
        let node = self.nodes.get(path).or_else(|| self.filter_nodes.get(path));
        self.focus.toggle_isolate(Some((path, rect.or_else(|| node.and_then(|n| n.rect)))));
        if rect.is_none() {
            if let Some(node) = node { Self::sight(&mut self.focus, node); }
        }
        self.focus_query()
    }

    pub fn mark_sent(&mut self, req: RequestId, query: &TreeQuery) {
        self.status.clear();
        match query {
            TreeQuery::Search { query, .. } => self.search_slot.mark_sent(req, query),
            TreeQuery::Reveal { path, .. } => {
                self.focus_reveal = (self.focus.isolated_path.as_ref() == Some(path)).then_some(req);
                self.reveal_slot.mark_sent(req, path);
            }
            TreeQuery::Children { path, .. } => {
                let parent = path.as_deref().unwrap_or("");
                self.loading.insert(parent.into());
                if path.is_none() { self.inflight_root = Some(req); }
                else { self.inflight_expand.insert(req, parent.into()); }
            }
        }
    }

    pub fn mark_send_failed(&mut self, _query: &TreeQuery, why: &str) {
        // In-flight and loading entries are installed only after a successful send.
        self.status = why.into();
    }

    /// Determine ownership and advance the relevant slot before parsing or displaying anything.
    fn finish(&mut self, req: RequestId) -> Option<(bool, Option<TreeQuery>)> {
        if self.retired.remove(&req) { return Some((false, None)) }
        if let Some((current, next)) = self.search_slot.finish(req) {
            return Some((current, next.map(|q| search(q, 0))))
        }
        if let Some((current, next)) = self.reveal_slot.finish(req) {
            return Some((current, next.map(reveal)))
        }
        if self.inflight_root == Some(req) {
            self.inflight_root = None;
            self.loading.remove("");
        } else {
            self.loading.remove(&self.inflight_expand.remove(&req)?);
        }
        Some((true, None))
    }

    pub fn apply_refusal(&mut self, req: RequestId, why: &str) -> Option<Option<TreeQuery>> {
        let focused = self.focus.isolated_path.as_ref().is_some_and(|path|
            self.reveal_slot.in_flight == Some(req) && self.reveal_slot.sent.as_ref() == Some(path)
                || self.inflight_expand.get(&req) == Some(path));
        let (current, next) = self.finish(req)?;
        if focused && current && why.starts_with("no widget at ") {
            self.focus.settle(TargetPresence::Missing);
        }
        if current && why != "Superseded by a newer tree query." { self.status = why.into(); }
        Some(next)
    }

    pub fn apply_reply(&mut self, req: RequestId, query: &TreeQuery, json: &str) -> Option<TreeQuery> {
        let (current, next) = self.finish(req)?;
        if !current { return next }
        let value = match super::parse_json(json) {
            Ok(v) => v,
            Err(why) => { self.status = why.into(); return None }
        };
        self.row_indices.clear();
        self.total_nodes = count(&value, "total_nodes");
        if let TreeQuery::Search { query, offset, .. } = query {
            if *offset == 0 { self.filter_pages.clear(); self.filter_nodes.clear(); }
            self.filter_result.clone_from(query);
            for (parent, page) in read_pages(&value) {
                for n in &page.nodes {
                    Self::sight(&mut self.focus, n);
                    let mut node = n.clone();
                    node.matched |= self.filter_nodes.get(&n.path).is_some_and(|old| old.matched);
                    self.filter_nodes.insert(n.path.clone(), node);
                }
                self.filter_pages.entry(parent).or_default().merge_matches(page);
            }
            self.filter_total_matches = count(&value, "total_matches");
            self.filter_loaded_matches = self.filter_pages.values().flat_map(|p| &p.nodes).filter(|n| n.matched)
                .count().min(self.filter_total_matches as usize) as u32;
            self.filter_truncated = value.get("truncated").and_then(Value::as_bool) == Some(true);
        } else {
            let initial = *query == Self::root();
            let focus_reveal = self.focus_reveal == Some(req);
            let revealed_target = matches!(query, TreeQuery::Reveal { path, .. } if Some(path) == self.focus.isolated_path.as_ref());
            let mut saw_target = false;
            for (parent, page) in read_pages(&value) {
                for n in &page.nodes {
                    saw_target |= self.focus.isolated_path.as_deref() == Some(&n.path);
                    Self::sight(&mut self.focus, n);
                    self.nodes.insert(n.path.to_string(), n.clone());
                    self.parent_of.insert(n.path.to_string(), parent.clone());
                }
                if initial && !self.refreshing { self.expanded.insert(parent.clone()); }
                self.pages.entry(parent).or_default().merge(page);
            }
            self.loaded |= initial;
            if initial { self.refreshing = false; }
            self.focus.settle(Self::target_presence(saw_target, revealed_target));
            if let TreeQuery::Reveal { path, .. } = query {
                // A geometry read may arrive after release. It never unfolds the old branch.
                if !focus_reveal { self.expand_cached_ancestors(path); }
            }
            return self.focus_query()
        }
        None
    }

    pub fn type_of(&self, path: &str) -> Option<&str> {
        self.nodes.get(path).or_else(|| self.filter_nodes.get(path)).map(|n| n.ty.as_str())
    }

    pub fn expand_cached_ancestors(&mut self, path: &str) -> bool {
        let mut child = path;
        let mut chain = HashSet::new();
        while !child.is_empty() {
            if self.focus.isolated_path.as_deref() == Some(child) { break }
            let Some(parent) = self.parent_of.get(child) else { return false };
            if !chain.insert(parent) || !self.pages.get(parent).is_some_and(|p| p.nodes.iter().any(|n| &*n.path == child)) {
                return false
            }
            child = parent;
        }
        if let Some(root) = self.focus.isolated_path.as_ref() {
            if child != root { return true }
            chain.remove(root);
        }
        self.expanded.extend(chain.into_iter().cloned());
        self.cancel_reveal();
        self.scroll_to = Some(path.into());
        true
    }

    pub fn sync_seats(&mut self, view: &SeatView, state: &State) -> bool {
        let mut dots = HashMap::<String, Vec<SeatDot>>::new();
        for (seat, s) in &view.selections {
            dots.entry(s.path.clone()).or_default().push(SeatDot { colour: seat_colour(state, seat), label: who(state, seat, s.agent) });
        }
        if dots == self.seat_dots { return false }
        self.seat_dots = dots;
        true
    }

    pub fn find_row_index(&self, path: &str) -> Option<usize> { self.row_indices.get(path).copied() }

    pub fn lines(&mut self, selected: &str) -> (Vec<TreeLine>, Option<usize>) {
        enum Visit<'a> { Parent(&'a str, u16), Node(&'a TreeNode, u16), Line(TreeLine) }
        let filtering = !self.filter.is_empty();
        let pages = if filtering { &self.filter_pages } else { &self.pages };
        let results_current = !filtering || self.filter_result == self.filter;
        let isolated = self.focus.isolated_path.as_deref();
        let mut stack = match isolated {
            Some(root) => {
                let node = if filtering { self.filter_nodes.get(root) } else { self.nodes.get(root) };
                node.filter(|_| results_current).map(|n| Visit::Node(n, 0)).into_iter().collect()
            }
            None => vec![Visit::Parent("", 0)],
        };
        let mut rows = Vec::new();
        self.row_indices.clear();
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Parent(parent, depth) => {
                    if !filtering && self.loading.contains(parent) {
                        stack.push(Visit::Line(TreeLine::Info { depth, text: "loading…".into() }));
                    }
                    if !results_current { continue }
                    let Some(page) = pages.get(parent) else { continue };
                    let end = page.end();
                    if end < page.total {
                        stack.push(Visit::Line(TreeLine::Pager { parent: parent.into(), depth, offset: end,
                            label: format!("+ {} more", page.total - end), is_search: false }));
                    }
                    stack.extend(page.nodes.iter().rev().map(|n| Visit::Node(n, depth)));
                    if page.offset > 0 {
                        rows.push(TreeLine::Pager { parent: parent.into(), depth, offset: page.offset.saturating_sub(TREE_PAGE.into()),
                            label: format!("↑ {} earlier", page.offset), is_search: false });
                    }
                }
                Visit::Node(node, depth) => {
                    let path = &node.path;
                    // The protocol validates each block individually. A parent can still point back to an ancestor.
                    let std::collections::hash_map::Entry::Vacant(index) = self.row_indices.entry(path.clone()) else { continue };
                    index.insert(rows.len());
                    let has_children = if filtering { pages.contains_key(&**path) } else { node.children > 0 };
                    let expanded = has_children && (filtering || isolated == Some(&**path) || self.expanded.contains(&**path));
                    let named = !node.name.is_empty() && node.name != "-" && !node.name.bytes().all(|b| b.is_ascii_digit());
                    let label = if named { format!("{} · {}", node.name, node.ty) } else { path.rsplit('/').next().unwrap_or(&node.ty).into() };
                    rows.push(TreeLine::Node { path: path.clone(), depth, label, ty: node.ty.clone(),
                        dim: !node.visible || (filtering && !node.matched), has_children, expanded,
                        selected: selected == &**path, even: rows.len() % 2 == 0,
                        drop: None, seats: self.seat_dots.get(&**path).cloned().unwrap_or_default() });
                    if expanded { stack.push(Visit::Parent(path, depth.saturating_add(1))); }
                }
                Visit::Line(line) => rows.push(line),
            }
        }
        if rows.is_empty() {
            let text = if (filtering && self.search_slot.in_flight.is_some()) || self.inflight_root.is_some()
                || isolated.is_some() && self.reveal_slot.in_flight.is_some() { "loading…" }
                else if !self.status.is_empty() { &self.status }
                else if filtering { "No matching widgets" } else { "No widgets" };
            rows.push(TreeLine::Info { depth: 0, text: text.into() });
        }
        if filtering && results_current && self.filter_truncated {
            rows.push(TreeLine::Pager { parent: "".into(), depth: 0, offset: self.filter_loaded_matches, is_search: true,
                label: format!("+ load more matches ({} of {})", self.filter_loaded_matches, self.filter_total_matches) });
        }
        let scroll = self.scroll_to.as_ref().and_then(|p| self.find_row_index(p));
        if scroll.is_some() { self.scroll_to = None; }
        (rows, scroll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(path: &str) -> TreeNode {
        TreeNode { path: path.into(), name: "-".into(), ty: "View".into(), children: 1, visible: true, matched: false, rect: None }
    }

    #[test]
    fn tree_state_isolate_scopes_subtree_and_restores_expanded() {
        const CARD: &str = "root.body.card";
        let data = |parents: &[(&str, &[(&str, u32)])]| {
            let blocks: Vec<_> = parents.iter().map(|(parent, nodes)| {
                let rows: Vec<_> = nodes.iter().map(|(path, children)| format!(
                    r#"{{"path":"{path}","ty":"View","name":"-","rect":[2,3,45,17],"visible":true,"children":{children}}}"#)).collect();
                format!(r#"{{"path":"{parent}","offset":0,"total":{},"nodes":[{}]}}"#, rows.len(), rows.join(","))
            }).collect();
            format!(r#"{{"window":2,"parents":[{}]}}"#, blocks.join(","))
        };
        let root = TreeState::root();
        let shallow = data(&[("", &[("root", 2)]), ("root", &[("body", 1), ("other", 0)]), ("body", &[(CARD, 1)])]);
        let children = data(&[(CARD, &[("title", 0)])]);
        let mut t = TreeState::default();
        t.mark_sent(1, &root);
        t.apply_reply(1, &root, &shallow);
        let expanded = t.expanded.clone();
        let full = t.lines("").0;
        let load = t.toggle_isolate(CARD, None).expect("a forced-open uncached branch loads its first page");
        t.mark_sent(2, &load);
        let (lines, _) = t.lines("");
        assert!(matches!(&lines[..], [TreeLine::Node { path, depth: 0, expanded: true, .. }, TreeLine::Info {depth: 1, text}]
            if path.as_ref() == CARD && text == "loading…"));
        t.apply_reply(2, &load, &children);
        let rows = |tree: &mut TreeState| tree.lines("").0.into_iter().filter_map(|line| match line {
            TreeLine::Node { path, depth, .. } => Some((path, depth)), _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(rows(&mut t), [(CARD.into(), 0), ("title".into(), 1)]);
        assert_eq!(t.focus.isolated_rect, Some(super::super::Rect { window: 2, x: 2., y: 3., w: 45., h: 17. }));
        let search = t.offer_search("View".into()).unwrap();
        t.mark_sent(3, &search);
        t.apply_reply(3, &search, &data(&[("body", &[(CARD, 1)]), (CARD, &[("title", 0)]), ("root", &[("other", 0)])]));
        assert_eq!(rows(&mut t), [(CARD.into(), 0), ("title".into(), 1)], "search excludes matches outside the branch");
        t.offer_search(String::new());
        t.toggle_isolate(CARD, None);
        assert_eq!(t.lines("").0, full, "clearing isolation restores the collapsed card and its siblings");
        assert_eq!(t.expanded, expanded);

        t.toggle_isolate(CARD, None);
        let root = t.clear_for_refresh();
        t.mark_sent(4, &root);
        let reveal = t.apply_reply(4, &root, &data(&[("", &[("root", 2)])])).expect("an omitted target needs a reveal");
        assert!(matches!(&reveal, TreeQuery::Reveal { path, .. } if path == CARD));
        assert_eq!(t.focus.isolated_path.as_deref(), Some(CARD));
        t.mark_sent(5, &reveal);
        let load = t.apply_reply(5, &reveal, &shallow).expect("reveal confirms the target but omits its children");
        t.mark_sent(6, &load);
        assert!(t.apply_reply(6, &load, &children).is_none());
        assert_eq!(rows(&mut t), [(CARD.into(), 0), ("title".into(), 1)]);
        assert_eq!(t.expanded, expanded);
        let isolated = t.focus.clone();
        for (req, why) in [(7, "Ask the owner to allow editing this share."), (8, "Allow revoked. Ask the owner again.")] {
            t.mark_sent(req, &reveal);
            assert_eq!(t.apply_refusal(req, why), Some(None));
            assert_eq!(t.focus, isolated, "a permission refusal must preserve isolation");
        }
        t.mark_sent(9, &reveal);
        assert!(t.apply_refusal(99, "no widget at that path").is_none());
        assert!(t.focus.rule().controls_enabled);
        t.apply_refusal(9, "no widget at that path");
        assert!(t.focus.isolated_path.is_none() && t.focus.hint.is_some());
    }

    #[test]
    fn pages_keep_order_across_pagers_and_reveal_windows() {
        let page = |offset, total, paths: &[&str]| ParentPage { offset, total, nodes: paths.iter().map(|p| node(p)).collect() };
        let mut p = page(2, 12, &["c", "d"]);
        p.merge(page(4, 12, &["e"]));
        p.merge(page(0, 12, &["a", "b"]));
        p.merge(page(1, 12, &["B", "C"]));
        let paths = |p: &ParentPage| p.nodes.iter().map(|n| n.path.to_string()).collect::<Vec<_>>();
        assert_eq!((p.offset, paths(&p)), (0, ["a","B","C","d","e"].map(String::from).to_vec()),
            "append, prepend and overlapping reveal preserve both tails and replace the overlap");
        p.merge(page(9, 12, &["j"]));
        assert_eq!(p.offset, 9, "a disjoint slice starts a new window");
        p.merge(page(9, 10, &["J"]));
        assert_eq!(paths(&p), ["J"], "a changed total replaces the previous snapshot");

        let mut t = TreeState::default();
        t.pages.insert("".into(), p);
        assert!(matches!(t.request_expand("", 0), Some(TreeQuery::Children {path: None, offset: 0, ..})),
            "the earlier root pager fetches offset zero without an empty wire path");
    }

    #[test]
    fn searches_and_reveals_coalesce_and_own_their_late_refusals() {
        let mut t = TreeState::default();
        let first = t.offer_search(" m\n".into()).unwrap();
        t.mark_sent(1, &first);
        assert!(t.offer_search("mo".into()).is_none() && t.offer_search("mov".into()).is_none(), "keystrokes wait for one search");
        let latest = t.apply_reply(1, &first, "").unwrap();
        assert_eq!(latest, search("mov".into(), 0), "the stale reply releases only the latest keystroke");
        t.mark_sent(2, &latest);
        assert!(t.offer_search(" \t".into()).is_none(), "clearing search is local");
        assert_eq!(t.apply_refusal(2, "late"), Some(None), "Escape still owns the outstanding refusal");
        assert!(t.status.is_empty() && t.filter_pages.is_empty(), "late refusal cannot overwrite the restored tree");
        assert_eq!(t.apply_refusal(3, "props"), None, "unrelated refusals fall through");

        let first = t.offer_search("move".into()).unwrap();
        t.mark_sent(7, &first);
        t.apply_reply(7, &first, r#"{"parents":[],"total_matches":2,"truncated":true}"#);
        let page = search("move".into(), 1);
        t.mark_sent(8, &page);
        t.offer_search(String::new());
        t.offer_search("move".into());
        assert_eq!(t.apply_reply(8, &page, ""), Some(first),
            "retyping the same search after Escape starts at page zero and discards the old continuation");

        let first = t.offer_reveal("/a").unwrap();
        t.mark_sent(4, &first);
        t.offer_reveal("/b");
        t.offer_reveal("/c");
        assert_eq!(t.apply_refusal(4, "superseded"), Some(Some(reveal("/c".into()))), "a newer pick survives a refused reveal");
        assert!(t.status.is_empty() && t.scroll_to.is_none(), "an old reveal cannot take focus or set an error");
    }

    #[test]
    fn walk_follows_cached_edges_and_survives_cycles_and_missing_pages() {
        let mut t = TreeState::default();
        let mut parent = String::new();
        for depth in 0..201 {
            let path = format!("/n{depth}");
            t.pages.insert(parent.clone(), ParentPage { offset: 0, total: 1, nodes: vec![node(&path)] });
            t.parent_of.insert(path.clone(), parent);
            parent = path;
        }
        assert!(t.expand_cached_ancestors(&parent), "the whole path is reachable");
        t.expanded.insert(parent.clone());
        t.loading.insert(parent.clone());
        let (rows, scroll) = t.lines(&parent);
        assert_eq!(scroll, Some(200), "follow scrolls to the exact deep row once");
        assert!(matches!(rows.last(), Some(TreeLine::Info {depth: 201, text}) if text == "loading…"), "first expand paints a loading child");
        assert!(matches!(&rows[200], TreeLine::Node {depth: 200, selected: true, label, ..} if label == "n200"), "unnamed widgets use the exact path segment");
        t.loading.clear();
        t.pages.insert(parent.clone(), ParentPage { offset: 0, total: 1, nodes: vec![node("/n0")] });
        assert_eq!(t.lines("").0.len(), 201, "a cyclic response cannot walk an ancestor twice");
        t.pages.remove("/n0");
        assert!(!t.expand_cached_ancestors(&parent), "detached cached nodes need a fresh reveal");
    }
}

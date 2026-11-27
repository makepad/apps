//! The interactive tree belongs to this seat. A search has its own pages so clearing it is local.
pub(super) mod view;
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

#[derive(Clone, Debug, PartialEq)]
pub(super) enum TreeLine {
    Node {
        path: Rc<str>, depth: u16, label: String, ty: String, dim: bool, has_children: bool,
        expanded: bool, selected: bool, even: bool, seats: Vec<SeatDot>,
        // T5 adds the drop position here.
    },
    Pager { parent: Rc<str>, depth: u16, offset: u32, label: String, is_search: bool },
    Info { depth: u16, text: String },
}

pub(super) enum TreeAction {
    Select(Rc<str>),
    Expand { path: Rc<str>, offset: u32 },
    Collapse(Rc<str>),
    Filter(String),
    LoadMoreMatches(u32),
    Refresh,
    // T5 adds structural actions here.
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
    pub loaded: bool,
    pub total_nodes: u32,
    pages: BTreeMap<String, ParentPage>,
    nodes: HashMap<String, TreeNode>,
    parent_of: HashMap<String, String>,
    expanded: BTreeSet<String>,
    loading: BTreeSet<String>,
    pub filter: String,
    filter_pages: BTreeMap<String, ParentPage>,
    filter_result: String,
    filter_total_matches: u32,
    filter_loaded_matches: u32,
    filter_truncated: bool,
    search_slot: LatestSlot,
    reveal_slot: LatestSlot,
    inflight_expand: HashMap<RequestId, String>,
    pub inflight_root: Option<RequestId>,
    retired: HashSet<RequestId>,
    pub scroll_to: Option<Rc<str>>,
    pub status: String,
    seat_dots: HashMap<String, Vec<SeatDot>>,
    row_indices: HashMap<Rc<str>, usize>,
}

fn search(query: String, offset: u32) -> TreeQuery { TreeQuery::Search { query, offset, limit: TREE_PAGE } }
fn reveal(path: String) -> TreeQuery { TreeQuery::Reveal { path, limit: TREE_PAGE } }

fn count(v: &Value, key: &str) -> u32 {
    v.get(key).and_then(Value::as_u64).unwrap_or(0).min(u32::MAX.into()) as u32
}

// These blocks have passed wire::validate_tree or the local session's valid_tree check.
fn read_pages(v: &Value) -> impl Iterator<Item = (String, ParentPage)> + '_ {
    v.get("parents").and_then(Value::as_arr).into_iter().flatten().filter_map(|p| {
        let parent = p.get("path")?.as_str()?.to_owned();
        let nodes = p.get("nodes")?.as_arr()?.iter().filter_map(|n| Some(TreeNode {
            path: n.get("path")?.as_str()?.into(), name: n.get("name")?.as_str()?.into(),
            ty: n.get("ty")?.as_str()?.into(), children: count(n, "children"),
            visible: n.get("visible")?.as_bool()?, matched: n.get("matched").and_then(Value::as_bool).unwrap_or(false),
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
        *self = Self { retired: std::mem::take(&mut self.retired), seat_dots: std::mem::take(&mut self.seat_dots),
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
        self.reveal_slot.cancel();
        self.scroll_to = None;
    }

    pub fn offer_reveal(&mut self, path: &str) -> Option<TreeQuery> {
        self.scroll_to = None;
        self.reveal_slot.offer(path.into()).map(reveal)
    }

    pub fn request_expand(&mut self, path: &str, offset: u32) -> Option<TreeQuery> {
        self.expanded.insert(path.into());
        if self.loading.contains(path) || self.pages.get(path).is_some_and(|p| {
            p.total == 0 || (p.offset <= offset && offset < p.end())
        }) { return None }
        Some(TreeQuery::Children { path: (!path.is_empty()).then(|| path.into()), depth: 1, offset, limit: TREE_PAGE })
    }

    pub fn collapse(&mut self, path: &str) { self.expanded.remove(path); }

    pub fn mark_sent(&mut self, req: RequestId, query: &TreeQuery) {
        self.status.clear();
        match query {
            TreeQuery::Search { query, .. } => self.search_slot.mark_sent(req, query),
            TreeQuery::Reveal { path, .. } => self.reveal_slot.mark_sent(req, path),
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
        let (current, next) = self.finish(req)?;
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
            if *offset == 0 { self.filter_pages.clear(); }
            self.filter_result.clone_from(query);
            for (parent, page) in read_pages(&value) { self.filter_pages.entry(parent).or_default().merge_matches(page); }
            self.filter_total_matches = count(&value, "total_matches");
            self.filter_loaded_matches = self.filter_pages.values().flat_map(|p| &p.nodes).filter(|n| n.matched)
                .count().min(self.filter_total_matches as usize) as u32;
            self.filter_truncated = value.get("truncated").and_then(Value::as_bool) == Some(true);
        } else {
            let initial = *query == Self::root();
            for (parent, page) in read_pages(&value) {
                for n in &page.nodes {
                    self.nodes.insert(n.path.to_string(), n.clone());
                    self.parent_of.insert(n.path.to_string(), parent.clone());
                }
                if initial { self.expanded.insert(parent.clone()); }
                self.pages.entry(parent).or_default().merge(page);
            }
            self.loaded |= initial;
            if let TreeQuery::Reveal { path, .. } = query { self.expand_cached_ancestors(path); }
        }
        None
    }

    pub fn type_of(&self, path: &str) -> Option<&str> {
        self.nodes.get(path).or_else(|| self.filter_pages.values().flat_map(|p| &p.nodes)
            .find(|n| n.path.as_ref() == path)).map(|n| n.ty.as_str())
    }

    pub fn expand_cached_ancestors(&mut self, path: &str) -> bool {
        let mut child = path;
        let mut chain = HashSet::new();
        while !child.is_empty() {
            let Some(parent) = self.parent_of.get(child) else { return false };
            if !chain.insert(parent) || !self.pages.get(parent).is_some_and(|p| p.nodes.iter().any(|n| &*n.path == child)) {
                return false
            }
            child = parent;
        }
        self.reveal_slot.cancel();
        self.expanded.extend(chain.into_iter().cloned());
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
        let mut stack = vec![Visit::Parent("", 0)];
        let results_current = !filtering || self.filter_result == self.filter;
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
                    let expanded = has_children && (filtering || self.expanded.contains(&**path));
                    let named = !node.name.is_empty() && node.name != "-" && !node.name.bytes().all(|b| b.is_ascii_digit());
                    let label = if named { format!("{} · {}", node.name, node.ty) } else { path.rsplit('/').next().unwrap_or(&node.ty).into() };
                    rows.push(TreeLine::Node { path: path.clone(), depth, label, ty: node.ty.clone(),
                        dim: !node.visible || (filtering && !node.matched), has_children, expanded,
                        selected: selected == &**path, even: rows.len() % 2 == 0,
                        seats: self.seat_dots.get(&**path).cloned().unwrap_or_default() });
                    if expanded { stack.push(Visit::Parent(path, depth.saturating_add(1))); }
                }
                Visit::Line(line) => rows.push(line),
            }
        }
        if rows.is_empty() {
            let text = if (filtering && self.search_slot.in_flight.is_some()) || self.inflight_root.is_some() { "loading…" }
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
        TreeNode { path: path.into(), name: "-".into(), ty: "View".into(), children: 1, visible: true, matched: false }
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

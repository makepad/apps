//! The slide's elements, source spans and picture selection. No second engine:
//! snapshots come from the current cut, on opening or explicit selection only.
use super::*;
use crate::cut::motion_edit::{picking, session::MotionSession};
use crate::structure::{StructureTree, TreeRow};
use makepad_motion::{schema::CodeFile, structure::{self, NodeKind}};
use makepad_story_edit_render::graphic::GraphicInspection;

#[derive(Clone, Debug)]
pub struct Element {
    pub key: String,
    pub path: String,
    pub file: String,
    pub label: String,
    pub line: usize,
    pub end_line: usize,
    pub span: std::ops::Range<usize>,
    pub quad: Option<[[f64; 2]; 4]>,
    pub depth: usize,
    pub kind: NodeKind,
}

impl Element {
    pub fn context(&self) -> String {
        format!("Selected · {} in {}:{}\nSelected node: {} (bytes {}..{}, lines {}..{}).", self.label, self.file, self.line, self.key, self.span.start, self.span.end, self.line, self.end_line)
    }
}

#[derive(Default)]
pub struct Selection {
    pub elements: Vec<Element>,
    picks: Vec<picking::Pickable>,
    owners: Vec<String>,
    clips: Vec<[f64; 4]>,
    output: [f64; 2],
    pub selected: Option<String>,
    pub revision: u64,
    pub ready: bool,
    folded: std::collections::HashSet<String>,
}

fn element(key: String, path: String, file: &str, label: String, source: &str, span: std::ops::Range<usize>, depth: usize, kind: NodeKind) -> Element {
    let line = |byte: usize| source[..byte.min(source.len())].bytes().filter(|b| *b == b'\n').count() + 1;
    Element { key, path, file: file.into(), label, line: line(span.start), end_line: line(span.end), span, quad: None, depth, kind }
}

fn source_spans(source: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    if let Ok(syntax) = makepad_script::syntax::parse(source) {
        syntax.walk(&mut |_, node| {
            if matches!(node.kind, makepad_script::syntax::Kind::Call { .. } | makepad_script::syntax::Kind::Object { .. }) { spans.push(node.span.clone()); }
        });
    }
    spans.sort_by_key(|span| span.len());
    spans
}

fn source_span(spans: &[std::ops::Range<usize>], byte: usize) -> std::ops::Range<usize> {
    spans.iter().find(|span| span.contains(&byte)).cloned().unwrap_or(byte..byte)
}

impl Selection {
    pub fn current(&self) -> Option<&Element> {
        self.selected.as_ref().and_then(|key| self.elements.iter().find(|e| e.key == *key))
    }

    pub fn select(&mut self, key: Option<String>) {
        if self.selected != key {
            if let Some(key) = &key { self.folded.retain(|parent| !key.starts_with(parent)); }
            self.selected = key;
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn fold(&mut self, key: String) {
        if !self.folded.remove(&key) { self.folded.insert(key); }
        self.revision = self.revision.wrapping_add(1);
    }

    pub fn pick(&mut self, at: [f64; 2], slop: f64) {
        let at = [at[0] * self.output[0], at[1] * self.output[1]];
        let hit = picking::pick(&self.picks, at, slop * self.output[0].max(self.output[1])).into_iter().find(|i| {
            let [x,y,w,h] = self.clips[*i];
            at[0] >= x && at[0] <= x+w && at[1] >= y && at[1] <= y+h
                && (self.picks[*i].content || self.picks[*i].area < 0.95 * self.output[0] * self.output[1])
        });
        self.select(hit.map(|i| self.owners[i].clone()));
    }

    pub fn rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        let mut hidden_below = None;
        for (i, e) in self.elements.iter().enumerate() {
            if hidden_below.is_some_and(|depth| e.depth > depth) { continue; }
            hidden_below = None;
            let children = self.elements.get(i + 1).is_some_and(|next| next.depth > e.depth);
            let open = children.then(|| !self.folded.contains(&e.key));
            if open == Some(false) { hidden_below = Some(e.depth); }
            rows.push(TreeRow { path: e.key.clone(), depth: e.depth, label: e.label.clone(), detail: format!("{}:{}", e.file, e.line), kind: e.kind, on: true, open });
        }
        rows
    }

    pub fn rebuild(&mut self, deck: &DeckDoc, index: usize, inspections: Vec<GraphicInspection>, placed: Vec<(String, [f64; 4])>) {
        let Some(slide) = deck.slides.get(index) else { return };
        let SlideCut::Inline(cut) = &slide.cut else { return };
        self.elements.clear(); self.picks.clear(); self.owners.clear(); self.clips.clear();
        let root = deck.tree.as_ref().map_or("deck.splash", |tree| tree.document.as_str());
        let cut_file = slide.graphic.as_deref().unwrap_or(root);
        let output = [cut.output.width as f64, cut.output.height as f64];
        self.output = output;
        let mut builtins = std::collections::HashMap::new();
        let mut spans = std::collections::HashMap::new();
        for inspection in inspections {
            let GraphicInspection { node, source, fit, size, placement, clip, doc, frame, boxes } = inspection;
            let builtin = !matches!(&source, makepad_story_cut::GraphicSrc::Tree(_));
            let file = match source { makepad_story_cut::GraphicSrc::Tree(tree) => tree.document, _ => cut_file.to_string() };
            let text = doc.source.text();
            let session = MotionSession::new(text.into(), doc.label.clone(), Some(node.clone()), None);
            let mut sites = picking::Sites::default();
            let picks = picking::pickables(&session, &doc, &frame, &boxes, &mut sites);
            let target = [size.0 as f64, size.1 as f64];
            let motion_fit = match fit {
                makepad_story_cut::Fit::Fill => makepad_motion::Fit::Fill,
                makepad_story_cut::Fit::Fit => makepad_motion::Fit::Fit,
                makepad_story_cut::Fit::None => makepad_motion::Fit::None,
            };
            let fitted = makepad_motion_draw::fit_transform(target, doc.size, motion_fit);
            let origin = fitted.apply([0.0, 0.0]);
            let scale = [fitted.apply([1.0, 0.0])[0] - origin[0], fitted.apply([0.0, 1.0])[1] - origin[1]];
            let transform = |quad: [[f64; 2]; 4]| quad.map(|p| {
                let p = fitted.apply(p);
                [(p[0] * placement[0] + placement[2]) / output[0], (p[1] * placement[1] + placement[3]) / output[1]]
            });
            let clip_quad = |quad: [[f64; 2]; 4]| quad.map(|p| [
                p[0].clamp(clip[0] / output[0], (clip[0] + clip[2]) / output[0]),
                p[1].clamp(clip[1] / output[1], (clip[1] + clip[3]) / output[1]),
            ]);
            if builtin && cut.nodes.iter().any(|candidate| candidate.id == node) {
                let mut bounds: Option<[f64; 4]> = None;
                for pick in picks.iter().filter(|pick| pick.content) {
                    for point in clip_quad(transform(pick.quad)) {
                        let b = bounds.get_or_insert([point[0],point[1],point[0],point[1]]);
                        b[0] = b[0].min(point[0]); b[1] = b[1].min(point[1]);
                        b[2] = b[2].max(point[0]); b[3] = b[3].max(point[1]);
                    }
                }
                if let Some([x,y,r,b]) = bounds { builtins.insert(node, [x,y,r-x,b-y]); }
                continue;
            }
            fn layers(rows: &mut Vec<Element>, n: &structure::Node, node: &str, file: &str, text: &str, depth: usize) {
                rows.push(element(format!("{node}/{}", n.path), n.path.clone(), file, n.label.clone(), text, n.range.clone(), depth, n.kind));
                for child in &n.children { layers(rows, child, node, file, text, depth + 1); }
            }
            if let Some(root) = structure::read(text) {
                for n in &root.children { layers(&mut self.elements, n, &node, &file, text, 0); }
            }
            for (i, mut pick) in picks.into_iter().enumerate() {
                let path = format!("Motion.{}", pick.path);
                let owner = self.elements.iter().rposition(|e| e.key.starts_with(&format!("{node}/")) && (path == e.path || path.starts_with(&format!("{}.", e.path)) || path.starts_with(&format!("{}#", e.path))));
                let mut key = owner.map(|i| self.elements[i].key.clone());
                let quad = clip_quad(transform(pick.quad));
                if let Some(owner) = owner {
                    if self.elements[owner].quad.is_none() || pick.ops.is_empty() { self.elements[owner].quad = Some(quad); }
                }
                if !pick.ops.is_empty() || owner.is_none() {
                    let (location_file, source_text, byte) = match pick.code.first() {
                        Some((CodeFile::Module(module), byte)) => (module.clone(), doc.loaded_module_text(module).map(|t| t.to_string()).unwrap_or_default(), *byte),
                        Some((CodeFile::Doc, byte)) => (file.clone(), text.to_string(), *byte),
                        None => (file.clone(), text.to_string(), 0),
                    };
                    let path = format!("{path}.draw[{i}]");
                    // A draw whose code site is not known (its canvas was
                    // not traced this time) points at its layer's code.
                    let (location_file, source_text, span) = match (pick.code.is_empty(), owner) {
                        (true, Some(owner)) => {
                            // A text it drew is a string literal in its layer's
                            // code: that call, else the layer.
                            let layer = self.elements[owner].span.clone();
                            // (Its layer's code first, then anywhere in the file: a
                            // `let` above the draw function.)
                            let quoted = pick.words.as_deref().map(str::trim).filter(|t| !t.is_empty()).map(|t| format!("\"{t}\""));
                            let at = quoted.and_then(|q| text.get(layer.clone()).and_then(|code| code.find(&q)).map(|i| layer.start + i).or_else(|| text.find(&q)));
                            let span = match at {
                                Some(byte) => source_span(spans.entry(file.clone()).or_insert_with(|| source_spans(text)), byte),
                                None => layer,
                            };
                            (self.elements[owner].file.clone(), text.to_string(), span)
                        }
                        _ => {
                            let span = source_span(spans.entry(location_file.clone()).or_insert_with(|| source_spans(&source_text)), byte);
                            (location_file, source_text, span)
                        }
                    };
                    let mut e = element(format!("{node}/{path}"), path, &location_file, pick.label.clone(), &source_text, span, owner.map_or(0, |i| self.elements[i].depth + 1), NodeKind::Layer);
                    e.quad = Some(quad); key = Some(e.key.clone());
                    let insert = owner.map_or(self.elements.len(), |i| {
                        let depth = self.elements[i].depth;
                        (i + 1..self.elements.len()).find(|j| self.elements[*j].depth <= depth).unwrap_or(self.elements.len())
                    });
                    self.elements.insert(insert, e);
                }
                let pixels = |q: [[f64;2];4]| transform(q).map(|p| [p[0]*output[0], p[1]*output[1]]);
                let point = |p: [f64;2]| pixels([p;4])[0];
                pick.quad = pixels(pick.quad); pick.area *= (scale[0] * placement[0] * scale[1] * placement[1]).abs();
                pick.seg = pick.seg.map(|(a,b,half)| (point(a), point(b), half * (scale[0] * placement[0]).abs().max((scale[1] * placement[1]).abs())));
                for (q, _) in &mut pick.glyphs { *q = pixels(*q); }
                if let Some(key) = key { self.picks.push(pick); self.owners.push(key); self.clips.push(clip); }
            }
        }
        for node in &cut.nodes {
            let (label, rect) = match &node.kind {
                CutNodeKind::AppView { app, rect, .. } => (format!("{} · app {app}", node.id), rect.unwrap_or([0.0, 0.0, 1.0, 1.0])),
                CutNodeKind::Code(spec) => (node.id.clone(), spec.rect.unwrap_or([0.0, 0.0, 1.0, 1.0])),
                CutNodeKind::Title { text, .. } => {
                    let Some(rect) = builtins.get(&node.id) else { continue };
                    (format!("{} · {text}", node.id), *rect)
                },
                CutNodeKind::Still { .. } | CutNodeKind::Clip { .. } | CutNodeKind::Camera(_) => (node.id.clone(), [0.0, 0.0, 1.0, 1.0]),
                _ => continue,
            };
            let line_start: usize = cut.source.split_inclusive('\n').take(node.loc.line.saturating_sub(1) as usize).map(str::len).sum();
            let byte = line_start + cut.source[line_start..].chars().take(node.loc.col.saturating_sub(1) as usize).map(char::len_utf8).sum::<usize>();
            let mut e = element(format!("cut/{}", node.id), node.id.clone(), cut_file, label, &cut.source, source_span(spans.entry(cut_file.to_string()).or_insert_with(|| source_spans(&cut.source)), byte), 0, NodeKind::Layer);
            let rect = placed.iter().find(|(id,_)| id == &node.id).map(|(_,r)| [r[0]/output[0],r[1]/output[1],r[2]/output[0],r[3]/output[1]]).unwrap_or(rect);
            let [x,y,w,h] = rect; let quad = [[x,y],[x+w,y],[x+w,y+h],[x,y+h]];
            e.quad = Some(quad);
            self.owners.push(e.key.clone());
            self.clips.push([0.0, 0.0, output[0], output[1]]);
            self.picks.push(picking::Pickable { quad: quad.map(|p| [p[0]*output[0],p[1]*output[1]]), area: w*h*output[0]*output[1], content: true, seg: None, code: Vec::new(), label: e.label.clone(), words: None, parent: None, glyphs: Vec::new(), path: e.path.clone(), ops: Vec::new(), loops: Vec::new() });
            self.elements.push(e);
        }
        // Give groups the union of their visible children, in linear tree order.
        let mut ancestors: Vec<usize> = Vec::new();
        let mut parents = Vec::with_capacity(self.elements.len());
        for (index, element) in self.elements.iter().enumerate() {
            while ancestors.last().is_some_and(|parent| self.elements[*parent].depth >= element.depth) { ancestors.pop(); }
            parents.push(ancestors.last().copied());
            ancestors.push(index);
        }
        for index in (0..self.elements.len()).rev() {
            let (Some(parent), Some(quad)) = (parents[index], self.elements[index].quad) else { continue };
            let bounds = self.elements[parent].quad.unwrap_or(quad).into_iter().chain(quad).fold(
                [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY],
                |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])],
            );
            let [x,y,r,b] = bounds;
            self.elements[parent].quad = Some([[x,y],[r,y],[r,b],[x,b]]);
        }
        self.ready = true;
        if self.selected.as_ref().is_some_and(|key| !self.elements.iter().any(|e| e.key == *key)) { self.selected = None; }
        self.revision = self.revision.wrapping_add(1);
    }
}

impl DeckPage {
    pub(crate) fn selected_element(&self) -> Option<&Element> { self.element.as_ref() }

    pub(crate) fn chat_context(&self) -> String {
        let Some(session) = &self.session else { return "The DECK page has no deck open.".into() };
        let names = session.doc.slides.iter().enumerate().map(|(i,s)| format!("{} {}", i+1, s.name)).collect::<Vec<_>>().join(", ");
        let mut context = format!("The DECK page: the deck {:?}, slides: {names}.", session.doc.title);
        if let Some(e) = &self.element { context.push('\n'); context.push_str(&e.context()); }
        if let Some(fancy) = self.fancy_context() { context.push('\n'); context.push_str(fancy); }
        context
    }

    pub(super) fn sync_structure(&mut self, cx: &mut Cx, ui: &WidgetRef) {
        let view = ui.widget(cx, ids!(deck_view));
        let Some(view) = view.borrow::<DeckView>() else { return };
        let selection = view.selection();
        if self.structure_seen == Some(selection.revision) { return; }
        self.structure_seen = Some(selection.revision);
        let rows = selection.rows(); let selected = selection.selected.clone();
        let element = selection.current().cloned(); drop(view);
        let selected_row = selected.as_ref().and_then(|selected| rows.iter().position(|row| &row.path == selected));
        if let Some(mut tree) = ui.widget(cx, ids!(deck_structure_tree)).borrow_mut::<StructureTree>() { tree.set_model(cx, rows, Vec::new(), selected); }
        // The model changes here after a draw has already rebuilt it (an edit
        // of a slide that is not animating): the tree's own redraw request
        // does not make a frame then, and the panel kept showing no rows
        // until something else drew it.
        cx.redraw_all();
        let changed = self.element.as_ref().map(|e| (&e.key,e.line)) != element.as_ref().map(|e| (&e.key,e.line));
        self.element = element;
        if changed {
            if let Some(row) = selected_row {
                // Structure rows are fixed height; scrolling is O(1).
                ui.view(cx, ids!(deck_structure)).set_scroll_pos(cx, dvec2(0.0, row.saturating_sub(2) as f64 * crate::structure::ROW_HEIGHT));
            }
            if let Some(e) = self.element.clone() {
                self.side_structure = true;
                self.panels_seen = None;
                ui.widget(cx, ids!(deck_side)).set_visible(cx, true);
                ui.widget(cx, ids!(deck_structure)).set_visible(cx, true);
                ui.widget(cx, ids!(deck_presenter)).set_visible(cx, false);
                if let Some(mut tabs) = ui.widget(cx, ids!(deck_side_tabs)).borrow_mut::<CutSegmented>() { tabs.set_active(cx, 1); }
                if self.open_file(cx, ui, &e.file).is_ok() {
                    self.pump_source(cx, ui, self.synced);
                    if let Some(pane) = &mut self.source { pane.reveal_span(cx, ui, e.span.clone()); }
                    ui.splitter(cx, ids!(deck_code_split)).set_collapse(cx, SplitterCollapse::None);
                }
            } else if let Some(pane) = &mut self.source {
                pane.clear_selection(cx, ui);
            }
        }
    }
}

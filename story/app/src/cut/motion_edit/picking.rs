//! From the picture to the code that drew it, and back.
//!
//! The desk evaluates its frames traced (`MotionDoc::eval_traced`) while
//! paused: every Draw2D op
//! carries where the draw function was when it drew it (the call site and
//! its callers). Here a frame becomes **pickables**: what was drawn, each
//! with its box in document units, whether it is content (text, strokes,
//! fills, shapes, images) or a container (a group, a full-frame shader or
//! canvas, a background), and the code it came from. A click picks the
//! smallest content under the pointer (containers only where nothing else
//! is); clicking the same spot again goes deeper down the pile. The same
//! records say which code draws at a moment ([`activity`]): the time
//! strip's ribbons and the "drawing now" glow.

use makepad_motion::plain::PrimitiveFrame;
use super::session::MotionSession;
use makepad_motion::draw2d::{Draw2dList, Op};
use makepad_motion::schema::{text_op_box, CodeFile};
use makepad_motion::{ItemKind, MotionDoc, MotionFrame};
use makepad_widgets::ScriptIp;
use std::collections::HashMap;

/// Something drawn that a click can pick.
#[derive(Clone, Debug)]
pub struct Pickable {
    /// Its box in document units (tl, tr, br, bl).
    pub quad: [[f64; 2]; 4],
    pub area: f64,
    /// Content (text, a stroke, a shape) rather than a container.
    pub content: bool,
    /// A stroke: its ends (document units) and half width, for a distance test.
    pub seg: Option<([f64; 2], [f64; 2], f64)>,
    /// The code that drew it: the file and the byte of its call site (or
    /// the string it shows), innermost first.
    pub code: Vec<(CodeFile, usize)>,
    /// What it is, said in a word (`c.text “Draw a unicorn…”`).
    pub label: String,
    /// The words it shows (a text), to find their literal.
    pub words: Option<String>,
    /// The canvas or layer it was drawn in (an Alt-click picks that).
    pub parent: Option<usize>,
    /// A text's glyphs (drawn one at a time, picked together): each one's
    /// box and the loops it was drawn in (`li` 0, `gi` 4): which iteration
    /// drew it, for where its character came from.
    pub glyphs: Vec<([[f64; 2]; 4], Vec<(makepad_widgets::LiveId, f64)>)>,
    /// The item that drew it and its Draw2D ops (none: the item as a
    /// whole): what the renderer's pick ids name (`MotionDraw::pick_name`).
    pub path: String,
    pub ops: Vec<usize>,
    /// The loops it was drawn in and their values then, outermost first
    /// (`s` 12): which instance of a generator it is.
    pub loops: Vec<(makepad_widgets::LiveId, f64)>,
}

/// Where call sites are, once asked (per model).
#[derive(Default)]
pub struct Sites {
    of: u64,
    map: std::collections::BTreeMap<ScriptIp, Option<(CodeFile, usize)>>,
}

impl Sites {
    /// The file and byte of each call site (cached per model revision).
    pub fn resolve(&mut self, session: &MotionSession, doc: &MotionDoc, ips: &[ScriptIp]) -> Vec<Option<(CodeFile, usize)>> {
        if self.of != session.doc_of {
            self.of = session.doc_of;
            self.map.clear();
        }
        let missing: Vec<ScriptIp> = ips.iter().copied().filter(|ip| !self.map.contains_key(ip)).collect();
        if !missing.is_empty() {
            let found = doc.code_at(&missing);
            for (ip, at) in missing.into_iter().zip(found) {
                // A place in the text the running document was loaded from,
                // moved to the text as it is now (a write since, a panel put
                // above, does not shift it onto another line).
                let byte = at.and_then(|at| {
                    let loaded;
                    let now = match text_of(session, &at.file) {
                        Some(text) => text,
                        None => {
                            let CodeFile::Module(key) = &at.file else { return None };
                            loaded = doc.loaded_module_text(key)?;
                            &loaded
                        }
                    };
                    let byte = match at.file {
                        CodeFile::Doc if doc.source.text() != now => moved(doc.source.text(), now, at.byte_in(doc.source.text())?)?,
                        _ => at.byte_in(now)?,
                    };
                    Some((at.file.clone(), byte))
                });
                self.map.insert(ip, byte);
            }
        }
        ips.iter().map(|ip| self.map.get(ip).cloned().flatten()).collect()
    }
}

/// Byte `at` of text `then` in text `now`: before the bytes that changed it
/// stays, after them it moves by the change; inside them it is gone.
pub fn moved(then: &str, now: &str, at: usize) -> Option<usize> {
    let p = then.bytes().zip(now.bytes()).take_while(|(a, b)| a == b).count();
    let room = then.len().min(now.len()) - p;
    let q = then.bytes().rev().zip(now.bytes().rev()).take(room).take_while(|(a, b)| a == b).count();
    if at < p {
        Some(at)
    } else if at >= then.len() - q {
        Some((at as isize + now.len() as isize - then.len() as isize) as usize)
    } else {
        None
    }
}

/// The text of a code file: the document's, or an open module's.
pub fn text_of<'a>(session: &'a MotionSession, file: &CodeFile) -> Option<&'a str> {
    match file {
        CodeFile::Doc => Some(&session.source),
        CodeFile::Module(key) => session.modules.iter().find(|m| m.key == *key).map(|m| m.source.as_str()),
    }
}

fn quad_of(m: &makepad_motion::Affine, b: [f64; 4]) -> [[f64; 2]; 4] {
    [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]].map(|p| m.apply(p))
}

fn quad_area(q: &[[f64; 2]; 4]) -> f64 {
    let mut a = 0.0;
    for i in 0..4 {
        let (p, n) = (q[i], q[(i + 1) % 4]);
        a += p[0] * n[1] - n[0] * p[1];
    }
    (a * 0.5).abs()
}

/// The ops of a Draw2D list, each with the mark it was drawn under.
fn marked<'a>(list: &'a Draw2dList) -> impl Iterator<Item = (&'a Op, usize, Option<&'a makepad_motion::draw2d::Mark>)> + 'a {
    list.ops.iter().enumerate().map(move |(i, op)| {
        let mark = list.marks.iter().rev().find(|m| (m.op as usize) <= i);
        (op, i, mark)
    })
}

/// Everything a frame drew that a click can pick, bottom first. `boxes`:
/// the layers' boxes as the viewer measures them (document units).
pub fn pickables(session: &MotionSession, doc: &MotionDoc, frame: &MotionFrame, boxes: &[(String, [[f64; 2]; 4])], sites: &mut Sites) -> Vec<Pickable> {
    let frame_area = (doc.size[0] * doc.size[1]).max(1.0);
    let mut out = Vec::new();
    for item in drawn_items(frame) {
        let layer_code = layer_code(session, &item.path);
        match &item.kind {
            ItemKind::Draw2d { picture, size } => {
                let m = item.transform(*size);
                let list = &picture.list;
                // The whole canvas: a container.
                let quad = quad_of(&m, [0.0, 0.0, size[0], size[1]]);
                let canvas = out.len();
                out.push(Pickable { area: quad_area(&quad), quad, content: false, seg: None, code: layer_code.clone(), label: "Draw2D".into(), words: None, parent: None, glyphs: Vec::new(), path: item.path.clone(), ops: Vec::new(), loops: Vec::new() });
                for (op, op_index, mark) in marked(list) {
                    let code = mark.map(|mark| sites.resolve(session, doc, &mark.sites).into_iter().flatten().collect::<Vec<_>>()).unwrap_or_default();
                    match op {
                        Op::Text(t) | Op::EraseText(t) => {
                            let b = text_op_box(t);
                            let tm = makepad_motion::Affine { a: t.m[0], b: t.m[1], c: t.m[2], d: t.m[3], e: t.m[4], f: t.m[5] };
                            let corners = [[b[0], b[1]], [b[2], b[1]], [b[2], b[3]], [b[0], b[3]]].map(|p| m.apply(tm.apply(p)));
                            // Glyphs drawn one at a time by the same code (a
                            // typewriter, a per-letter animation) are one pick:
                            // the word or line they spell, around all of them.
                            // (Only letters that run on along one line: the
                            // same call drawing separate texts, a note each,
                            // draws separate picks.)
                            let runs_on = |p: &Pickable| {
                                let Some(last) = p.glyphs.last().map(|(q, _)| *q) else { return false };
                                let (ly0, ly1) = (last.iter().map(|c| c[1]).fold(f64::MAX, f64::min), last.iter().map(|c| c[1]).fold(f64::MIN, f64::max));
                                let (cy0, cy1) = (corners.iter().map(|c| c[1]).fold(f64::MAX, f64::min), corners.iter().map(|c| c[1]).fold(f64::MIN, f64::max));
                                let h = (ly1 - ly0).abs().max(1e-6);
                                let overlap = (ly1.min(cy1) - ly0.max(cy0)) / h;
                                let last_right = last.iter().map(|c| c[0]).fold(f64::MIN, f64::max);
                                let gap = corners.iter().map(|c| c[0]).fold(f64::MAX, f64::min) - last_right;
                                overlap > 0.5 && gap > -h * 0.5 && gap < h * 1.5
                            };
                            if let Some(prev) = out.last_mut().filter(|p| p.words.is_some() && p.parent == Some(canvas) && p.code == code && p.seg.is_none() && runs_on(p)) {
                                let prev_right = prev.quad.iter().map(|c| c[0]).fold(f64::MIN, f64::max);
                                let pts: Vec<[f64; 2]> = prev.quad.iter().chain(corners.iter()).copied().collect();
                                if let Some(q) = bbox(&pts) {
                                    prev.quad = q;
                                    prev.area = quad_area(&q);
                                }
                                // A gap wider than a sixth of the letters'
                                // height between them is a space.
                                let height = (prev.quad[2][1] - prev.quad[0][1]).abs().max(1e-6);
                                let gap = corners.iter().map(|c| c[0]).fold(f64::MAX, f64::min) - prev_right;
                                prev.glyphs.push((corners, mark.map(|m| m.loops.clone()).unwrap_or_default()));
                                prev.ops.push(op_index);
                                if let Some(w) = prev.words.as_mut() {
                                    if gap > height * 0.16 && !w.ends_with(' ') && !t.text.starts_with(' ') {
                                        w.push(' ');
                                    }
                                    w.push_str(&t.text);
                                    let short: String = w.chars().take(24).collect();
                                    prev.label = format!("text “{short}”");
                                }
                                continue;
                            }
                            let short: String = t.text.chars().take(24).collect();
                            let glyphs = vec![(corners, mark.map(|m| m.loops.clone()).unwrap_or_default())];
                            out.push(Pickable { area: quad_area(&corners), quad: corners, content: true, seg: None, code, label: format!("text “{short}”"), words: Some(t.text.clone()), parent: Some(canvas), glyphs, path: item.path.clone(), ops: vec![op_index], loops: mark.map(|m| m.loops.clone()).unwrap_or_default() });
                        }
                        Op::Segs { start, end, .. } => {
                            // Each stroke on its own: the pick tests its distance.
                            for (k, s) in list.segs[*start..*end].iter().enumerate() {
                                let seg_index = (*start + k) as u32;
                                let mk = list.marks.iter().rev().find(|mk| mk.seg <= seg_index);
                                let code = mk.map(|mk| sites.resolve(session, doc, &mk.sites).into_iter().flatten().collect::<Vec<_>>()).unwrap_or_else(|| code.clone());
                                let loops = mk.map(|mk| mk.loops.clone()).unwrap_or_default();
                                let (a, b) = (m.apply([s.a[0] as f64, s.a[1] as f64]), m.apply([s.b[0] as f64, s.b[1] as f64]));
                                let scale = (m.a * m.a + m.b * m.b).sqrt().max(1e-6);
                                let half = (s.width as f64 * scale * 0.5).max(0.5);
                                let quad = [[a[0].min(b[0]) - half, a[1].min(b[1]) - half], [a[0].max(b[0]) + half, a[1].min(b[1]) - half], [a[0].max(b[0]) + half, a[1].max(b[1]) + half], [a[0].min(b[0]) - half, a[1].max(b[1]) + half]];
                                let len = ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
                                out.push(Pickable { area: (len * half * 2.0).max(1.0), quad, content: s.color[3] > 0.05, seg: Some((a, b, half)), code, label: "stroke".into(), words: None, parent: Some(canvas), glyphs: Vec::new(), path: item.path.clone(), ops: vec![op_index], loops });
                            }
                        }
                        Op::Fill { contours, color } => {
                            let pts: Vec<[f64; 2]> = contours.iter().flatten().map(|p| m.apply(*p)).collect();
                            if let Some(q) = bbox(&pts) {
                                let area = quad_area(&q);
                                out.push(Pickable { area, quad: q, content: color[3] > 0.05 && area < frame_area * 0.6, seg: None, code, label: "fill".into(), words: None, parent: Some(canvas), glyphs: Vec::new(), path: item.path.clone(), ops: vec![op_index], loops: mark.map(|m| m.loops.clone()).unwrap_or_default() });
                            }
                        }
                        Op::Gradient { contours, .. } => {
                            let pts: Vec<[f64; 2]> = contours.iter().flatten().map(|p| m.apply(*p)).collect();
                            if let Some(q) = bbox(&pts) {
                                let area = quad_area(&q);
                                out.push(Pickable { area, quad: q, content: area < frame_area * 0.6, seg: None, code, label: "gradient".into(), words: None, parent: Some(canvas), glyphs: Vec::new(), path: item.path.clone(), ops: vec![op_index], loops: mark.map(|m| m.loops.clone()).unwrap_or_default() });
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                let Some((_, quad)) = boxes.iter().find(|(path, _)| *path == item.path) else { continue };
                let area = quad_area(quad);
                // A layer whose words are its `text` (kinetic type, 3D text):
                // those words, as content.
                let layer_words = session.outline.all().into_iter().filter(|l| item.path == l.path || item.path.starts_with(&format!("{}#", l.path))).max_by_key(|l| l.path.len())
                    .and_then(|l| session.value_site(&l.path, "text")).and_then(|site| session.resolved(site).words());
                let (content, label, words) = match &item.kind {
                    ItemKind::Primitive(PrimitiveFrame::Text { text, .. }) => (true, format!("text “{}”", text.chars().take(24).collect::<String>()), Some(text.clone())),
                    _ if layer_words.is_some() => (true, format!("text “{}”", layer_words.as_deref().unwrap_or("").chars().take(24).collect::<String>()), layer_words.clone()),
                    ItemKind::Group { .. } | ItemKind::Shader { .. } | ItemKind::Scene3D { .. } | ItemKind::Scene3dDoc { .. } => (false, "layer".to_string(), None),
                    _ => (item.opacity > 0.05 && area < frame_area * 0.6, "shape".to_string(), None),
                };
                out.push(Pickable { area, quad: *quad, content, seg: None, code: layer_code, label, words, parent: None, glyphs: Vec::new(), path: item.path.clone(), ops: Vec::new(), loops: Vec::new() });
            }
        }
    }
    out
}

/// Every item a frame draws: its layers, and the layers drawn into data
/// textures for its shaders (a sheet's drawing, composited by a full-frame
/// shader: what is seen is theirs, not the shader's).
fn drawn_items(frame: &MotionFrame) -> Vec<&makepad_motion::Item> {
    fn go<'a>(items: &'a [makepad_motion::Item], out: &mut Vec<&'a makepad_motion::Item>) {
        for item in items {
            out.push(item);
            if let ItemKind::Group { children, mask, .. } = &item.kind {
                go(children, out);
                if let Some(mask) = mask {
                    go(std::slice::from_ref(mask), out);
                }
            }
            if let ItemKind::Fly { inner, .. } | ItemKind::Anchored { inner, .. } = &item.kind {
                go(std::slice::from_ref(inner.as_ref()), out);
            }
            // A Scene3D's pictures (its `layers:`): what it shows on its
            // surfaces and passes (the renderer names them through them).
            if let ItemKind::Scene3dDoc { layers, .. } = &item.kind {
                for (_, items) in layers {
                    go(items, out);
                }
            }
        }
    }
    let mut out: Vec<&makepad_motion::Item> = Vec::new();
    for (_, items) in &frame.data_layers {
        go(items, &mut out);
    }
    go(&frame.items, &mut out);
    out
}

fn bbox(pts: &[[f64; 2]]) -> Option<[[f64; 2]; 4]> {
    let first = pts.first()?;
    let (mut lo, mut hi) = (*first, *first);
    for p in pts {
        lo = [lo[0].min(p[0]), lo[1].min(p[1])];
        hi = [hi[0].max(p[0]), hi[1].max(p[1])];
    }
    Some([[lo[0], lo[1]], [hi[0], lo[1]], [hi[0], hi[1]], [lo[0], hi[1]]])
}

/// A layer's own code: where its constructor is written.
fn layer_code(session: &MotionSession, path: &str) -> Vec<(CodeFile, usize)> {
    let owner = session.outline.all().into_iter().filter(|l| path == l.path || path.starts_with(&format!("{}.", l.path)) || path.starts_with(&format!("{}#", l.path))).max_by_key(|l| l.path.len());
    owner.map(|l| vec![(CodeFile::Doc, l.range.start)]).unwrap_or_default()
}

fn inside(q: &[[f64; 2]; 4], p: [f64; 2]) -> bool {
    let mut sign = 0.0;
    for i in 0..4 {
        let (a, b) = (q[i], q[(i + 1) % 4]);
        let cross = (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
        if cross.abs() < 1e-9 {
            continue;
        }
        if sign == 0.0 {
            sign = cross.signum();
        } else if cross.signum() != sign {
            return false;
        }
    }
    true
}

/// What lies under `p` (document units; `slop` a few pixels in document
/// units), best first: content by size, then containers.
pub fn pick(list: &[Pickable], p: [f64; 2], slop: f64) -> Vec<usize> {
    let mut hits: Vec<usize> = (0..list.len()).filter(|&i| {
        let it = &list[i];
        match it.seg {
            Some((a, b, half)) => {
                let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                let len2 = (dx * dx + dy * dy).max(1e-12);
                let u = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len2).clamp(0.0, 1.0);
                let (cx, cy) = (a[0] + u * dx, a[1] + u * dy);
                ((p[0] - cx).powi(2) + (p[1] - cy).powi(2)).sqrt() <= half + slop
            }
            None => {
                let grown = grow(&it.quad, slop);
                inside(&grown, p)
            }
        }
    }).collect();
    // A text whose letters are under the point comes first: it is what one
    // reads there (the grid or an axis under a listing is behind it).
    let text_under = |it: &Pickable| it.words.is_some() && (it.glyphs.iter().any(|(q, _)| inside(&grow(q, slop), p)) || (it.glyphs.is_empty() && inside(&it.quad, p)));
    hits.sort_by(|&a, &b| {
        let (x, y) = (&list[a], &list[b]);
        text_under(y).cmp(&text_under(x)).then(y.content.cmp(&x.content)).then(x.area.partial_cmp(&y.area).unwrap_or(std::cmp::Ordering::Equal)).then(b.cmp(&a))
    });
    hits
}

/// What the renderer says drew under the pointer (`names`, nearest first:
/// an item's path and Draw2D op, `MotionDraw::pick_name`), as picks: each
/// name's things, those the point is on by shape first (a stroke op names
/// all its strokes). Only what the renderer names: through warps, lenses
/// and a picture on a 3D surface it is what the picture shows there, where
/// the shapes are only where the code put them.
pub fn pick_named(list: &[Pickable], names: &[(String, Option<usize>)], p: [f64; 2], slop: f64) -> Vec<usize> {
    let mut out: Vec<usize> = Vec::new();
    let by_shape = pick(list, p, slop);
    for (path, op) in names {
        // A name without an op is the layer as a whole, or a run of its
        // drawing named together (its fills): the layer, and its things
        // under the point by shape.
        let mut named: Vec<usize> = (0..list.len()).filter(|&i| {
            let it = &list[i];
            it.path == *path && match op {
                Some(op) => it.ops.contains(op),
                None => it.ops.is_empty() || by_shape.contains(&i),
            }
        }).collect();
        // A stroke op names all its strokes: those the point is on first
        // (a warped picture's point may be on none), in shape order.
        named.sort_by_key(|i| by_shape.iter().position(|j| j == i).unwrap_or(usize::MAX));
        for i in named {
            if !out.contains(&i) {
                out.push(i);
            }
        }
    }
    out
}

fn grow(q: &[[f64; 2]; 4], by: f64) -> [[f64; 2]; 4] {
    let c = [(q[0][0] + q[2][0]) * 0.5, (q[0][1] + q[2][1]) * 0.5];
    q.map(|p| {
        let (dx, dy) = (p[0] - c[0], p[1] - c[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-9);
        [p[0] + dx / len * by, p[1] + dy / len * by]
    })
}

/// How much each code region draws in a frame, per file: by region index
/// of that file's code outline (and every region holding it).
pub fn activity(session: &MotionSession, doc: &MotionDoc, frame: &MotionFrame, sites: &mut Sites) -> HashMap<CodeFile, Vec<f32>> {
    let mut out: HashMap<CodeFile, Vec<f32>> = HashMap::new();
    let add = |out: &mut HashMap<CodeFile, Vec<f32>>, file: &CodeFile, byte: usize, amount: f32| {
        let code = match file {
            CodeFile::Doc => &session.code,
            CodeFile::Module(key) => match session.modules.iter().find(|m| m.key == *key) {
                Some(m) => &m.code,
                None => return,
            },
        };
        let row = out.entry(file.clone()).or_insert_with(|| vec![0.0; code.regions.len()]);
        if row.len() != code.regions.len() {
            row.resize(code.regions.len(), 0.0);
        }
        let mut r = code.region_at(byte);
        while let Some(i) = r {
            row[i] += amount;
            r = code.regions[i].parent;
        }
    };
    for item in drawn_items(frame) {
        match &item.kind {
            ItemKind::Draw2d { picture, .. } => {
                let list = &picture.list;
                for (k, mark) in list.marks.iter().enumerate() {
                    let next = list.marks.get(k + 1);
                    let ops = next.map_or(list.ops.len() as u32, |n| n.op).saturating_sub(mark.op);
                    let segs = next.map_or(list.segs.len() as u32, |n| n.seg).saturating_sub(mark.seg);
                    let amount = (ops + segs) as f32;
                    if amount <= 0.0 {
                        continue;
                    }
                    // Each file once per mark (a recursion counts once).
                    let mut seen: Vec<(CodeFile, usize)> = Vec::new();
                    for (file, byte) in sites.resolve(session, doc, &mark.sites).into_iter().flatten() {
                        if !seen.iter().any(|(f, b)| *f == file && *b == byte) {
                            add(&mut out, &file, byte, amount);
                            seen.push((file, byte));
                        }
                    }
                }
            }
            _ => {
                for (file, byte) in layer_code(session, &item.path) {
                    add(&mut out, &file, byte, 1.0);
                }
            }
        }
    }
    out
}

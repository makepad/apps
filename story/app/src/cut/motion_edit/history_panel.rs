//! The HISTORY tab beside STRUCTURE: every state the open graphic has been
//! in, newest first, one row per change (a gesture is one), said in the
//! document's own names, with who made it and when. A click takes the
//! document to that state (itself a new entry: the history only grows;
//! Cmd-Z returns); the pointer over a row shows the code it touched.
//!
//! The history is the session's (`MotionSession::history`), kept beside the
//! person's drafts so it survives a restart. The agent reads the same
//! history (`history.rs`: listing, read, diff, merge), and the row picked
//! here is its step `picked`.

use super::super::VideoCut;
use super::session::{HistoryStep, Snapshot, StepKind};
use makepad_value_pickers::panel::control::{fit_text, DrawPanelShape, R_CONTROL};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    mod.widgets.HistoryListBase = #(HistoryList::register_widget(vm))
    /** The history's rows, newest first. */
    mod.widgets.HistoryList = set_type_default() do mod.widgets.HistoryListBase{
        width: Fill height: Fit
        draw_text +: {text_style: theme.font_regular{font_size: 8.5}}
        draw_small +: {text_style: theme.font_regular{font_size: 7.5}}
    }

    mod.widgets.HistoryColumn = ScrollYView{width: Fill height: Fill flow: Down
        h_list := mod.widgets.HistoryList{}
        View{width: Fill height: 16}
    }
}

/// A row as drawn.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub index: usize,
    pub label: String,
    pub detail: String,
    pub current: bool,
    pub selected: bool,
    /// A step the document came back from (an undo, a "back to").
    pub nav: bool,
}

#[derive(Clone, Debug, Default)]
pub enum HistoryAction {
    #[default]
    None,
    Go(usize),
    Hover(Option<usize>),
}

#[derive(Script, ScriptHook, Widget)]
pub struct HistoryList {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_bg: DrawColor,
    #[live]
    draw_shape: DrawPanelShape,
    #[live]
    draw_text: DrawText,
    #[live]
    draw_small: DrawText,
    #[rust]
    rows: Vec<Row>,
    #[rust]
    spots: Vec<(Rect, usize)>,
    #[rust]
    hot: Option<usize>,
}

const ROW: f64 = 22.0;

fn color(value: u32, alpha: f32) -> Vec4f {
    let mut c = Vec4f::from_u32(value);
    c.w *= alpha;
    c
}

impl HistoryList {
    pub fn set_rows(&mut self, cx: &mut Cx, rows: Vec<Row>) {
        if self.rows != rows {
            self.rows = rows;
            self.draw_bg.redraw(cx);
        }
    }
}

impl Widget for HistoryList {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let uid = self.widget_uid();
        let at = |s: &Self, p: DVec2| s.spots.iter().find(|(r, _)| r.contains(p)).map(|(_, i)| *i);
        match event.hits(cx, self.draw_bg.area()) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let hot = at(self, fe.abs);
                if hot != self.hot {
                    self.hot = hot;
                    cx.widget_action(uid, HistoryAction::Hover(hot));
                    self.draw_bg.redraw(cx);
                }
                cx.set_cursor(if hot.is_some() { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) => {
                self.hot = None;
                cx.widget_action(uid, HistoryAction::Hover(None));
                self.draw_bg.redraw(cx);
            }
            Hit::FingerUp(fe) if fe.is_over && fe.is_primary_hit() => {
                if let Some(i) = at(self, fe.abs) {
                    cx.widget_action(uid, HistoryAction::Go(i));
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        let p = makepad_look::palette::get(cx);
        let height = 6.0 + self.rows.len() as f64 * ROW;
        let rect = cx.walk_turtle(Walk { height: Size::Fixed(height), ..walk });
        self.draw_bg.color = color(0, 0.0);
        self.draw_bg.draw_abs(cx, rect);
        self.spots.clear();
        let mut y = rect.pos.y + 4.0;
        for row in &self.rows {
            let r = Rect { pos: dvec2(rect.pos.x + 6.0, y), size: dvec2(rect.size.x - 12.0, ROW - 2.0) };
            let hot = self.hot == Some(row.index);
            if row.current {
                self.draw_shape.fill_round(cx, r, color(p.accent_wash, 1.0), R_CONTROL);
            } else if hot {
                self.draw_shape.fill_round(cx, r, color(p.control_hover, 1.0), R_CONTROL);
            }
            if row.selected && !row.current {
                self.draw_shape.edged_round(cx, r, color(0, 0.0), color(p.accent, 0.8), 1.0, R_CONTROL);
            }
            let detail_w = makepad_widgets::badge::advance(&self.draw_small, cx, &row.detail).min(r.size.x * 0.45);
            self.draw_text.color = color(if row.nav { p.muted } else if row.current { p.text_hi } else { p.text }, 1.0);
            let (label, _) = fit_text(&self.draw_text, cx, &row.label, (r.size.x - detail_w - 18.0).max(20.0));
            self.draw_text.draw_abs(cx, dvec2(r.pos.x + 8.0, r.pos.y + 4.0), &label);
            self.draw_small.color = color(p.dim, 1.0);
            let (detail, _) = fit_text(&self.draw_small, cx, &row.detail, detail_w);
            self.draw_small.draw_abs(cx, dvec2(r.pos.x + r.size.x - detail_w - 6.0, r.pos.y + 5.0), &detail);
            self.spots.push((r, row.index));
            y += ROW;
        }
        DrawStep::done()
    }
}

// ---------------------------------------------------------------- the desk's side

/// The tab's state on the page.
#[derive(Default)]
pub struct History {
    /// The history row the person picked last (the agent reads it).
    pub selected: Option<usize>,
    /// The session the history was read back for (its key).
    restored: Option<String>,
    /// What was last written to the file (entries, the last one's time).
    saved: (usize, f64),
    shown: Option<(usize, Option<usize>, Option<usize>, u64)>,
}

/// Where a graphic's history is kept: beside the person's drafts.
fn file_of(key: &str) -> std::path::PathBuf {
    let safe: String = key.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    makepad_app_shell::user_state::dir(None, "motion-history").join(format!("{safe}.json"))
}

fn session_key(page: &VideoCut) -> Option<String> {
    let s = page.motion.desk.session.as_ref()?;
    Some(s.asset.as_ref().map(|a| a.asset_id.to_string()).unwrap_or_else(|| format!("{}-{}", s.title, s.node.clone().unwrap_or_default())))
}

fn kind_text(kind: &StepKind) -> String {
    match kind {
        StepKind::Opened => "opened".into(),
        StepKind::Edit => "edit".into(),
        StepKind::Undo => "undo".into(),
        StepKind::Redo => "redo".into(),
        StepKind::GoTo(n) => format!("goto:{n}"),
    }
}

fn kind_of(text: &str) -> StepKind {
    match text {
        "opened" => StepKind::Opened,
        "undo" => StepKind::Undo,
        "redo" => StepKind::Redo,
        t => t.strip_prefix("goto:").and_then(|n| n.parse().ok()).map(StepKind::GoTo).unwrap_or(StepKind::Edit),
    }
}

/// The history as JSON: the texts once each (a state repeats them), the
/// steps naming them.
fn to_json(steps: &[HistoryStep]) -> String {
    let mut texts: Vec<String> = Vec::new();
    let mut id = |t: &str| -> i64 {
        match texts.iter().position(|x| x == t) {
            Some(i) => i as i64,
            None => {
                texts.push(t.to_string());
                (texts.len() - 1) as i64
            }
        }
    };
    let rows: Vec<Json> = steps.iter().map(|s| {
        let modules: Vec<Json> = s.after.modules.iter().map(|(k, t)| Json::Arr(vec![json::s(k.clone()), Json::Int(id(t))])).collect();
        json::obj(vec![
            ("label", json::s(s.label.clone())),
            ("source", json::s(s.source.clone())),
            ("time", Json::F64(s.time)),
            ("kind", json::s(kind_text(&s.kind))),
            ("text", Json::Int(id(&s.after.source))),
            ("modules", Json::Arr(modules)),
            ("range", s.range.as_ref().map_or(Json::Null, |r| Json::Arr(vec![Json::Int(r.start as i64), Json::Int(r.end as i64)]))),
        ])
    }).collect();
    let doc = json::obj(vec![("texts", Json::Arr(texts.into_iter().map(json::s).collect())), ("steps", Json::Arr(rows))]);
    let mut out = String::new();
    doc.write_into(&mut out);
    out
}

fn from_json(text: &str) -> Option<Vec<HistoryStep>> {
    let doc = json::parse(text.as_bytes()).ok()?;
    let texts: Vec<String> = doc.get("texts")?.as_arr()?.iter().map(|t| t.as_str().unwrap_or("").to_string()).collect();
    let text = |v: Option<&Json>| v.and_then(|v| v.as_i64()).and_then(|i| texts.get(i as usize)).cloned();
    doc.get("steps")?.as_arr()?.iter().map(|s| {
        let modules = s.get("modules").and_then(|m| m.as_arr()).map(|m| m.iter().filter_map(|pair| {
            let pair = pair.as_arr()?;
            Some((pair.first()?.as_str()?.to_string(), text(pair.get(1))?))
        }).collect()).unwrap_or_default();
        let range = s.get("range").and_then(|r| r.as_arr()).and_then(|r| Some(r.first()?.as_u64()? as usize..r.get(1)?.as_u64()? as usize));
        let time = match s.get("time") {
            Some(Json::F64(f)) => *f,
            Some(Json::Int(i)) => *i as f64,
            _ => 0.0,
        };
        Some(HistoryStep {
            label: s.get("label")?.as_str()?.to_string(),
            source: s.get("source").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            time,
            kind: kind_of(s.get("kind").and_then(|v| v.as_str()).unwrap_or("edit")),
            after: Snapshot { source: text(s.get("text"))?, modules },
            range,
        })
    }).collect()
}

/// "14:32", or "yesterday 14:32", or a date.
fn when(t: f64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let ago = now - t;
    if ago < 60.0 {
        "now".into()
    } else if ago < 3600.0 {
        format!("{} min", (ago / 60.0).floor())
    } else if ago < 86400.0 {
        format!("{} h", (ago / 3600.0).floor())
    } else {
        format!("{} d", (ago / 86400.0).floor())
    }
}

/// Every pump: the history read back once per graphic, the steps' sources
/// filled from who made them, the file kept, the list drawn.
pub fn pump(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let Some(key) = session_key(page) else { return };
    // Read back: when the file's last state is the document as it opened,
    // the session's history continues it.
    if page.motion.history.restored.as_deref() != Some(key.as_str()) {
        page.motion.history.restored = Some(key.clone());
        page.motion.history.selected = None;
        page.motion.history.saved = (0, 0.0);
        if let Some(old) = std::fs::read_to_string(file_of(&key)).ok().and_then(|t| from_json(&t)) {
            if let Some(session) = page.motion.desk.session.as_mut() {
                if old.last().is_some_and(|l| l.after.source == session.source) && session.history.len() <= 1 {
                    session.history = old;
                }
            }
        }
    }
    let author = page.motion.desk.author.clone();
    let Some(session) = page.motion.desk.session.as_mut() else { return };
    // Who made the newest steps.
    if let Some(author) = author {
        for step in session.history.iter_mut().rev().take_while(|s| s.source.is_empty() && s.kind != StepKind::Opened) {
            step.source = author.clone();
        }
    }
    // Kept a moment after it changes.
    let last_time = session.history.last().map_or(0.0, |s| s.time);
    let st = &mut page.motion.history;
    if (session.history.len(), last_time) != st.saved {
        st.saved = (session.history.len(), last_time);
        let file = file_of(&key);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(file, to_json(&session.history));
    }
    let current = session.current_step();
    let key_now = (session.history.len(), current, st.selected, session.revision);
    if st.shown == Some(key_now) {
        return;
    }
    st.shown = Some(key_now);
    let rows: Vec<Row> = session.history.iter().enumerate().rev().map(|(i, s)| {
        let who = s.source.split(" · ").last().unwrap_or("").to_string();
        let detail = if who.is_empty() { when(s.time) } else { format!("{who} · {}", when(s.time)) };
        Row { index: i, label: s.label.clone(), detail, current: current == Some(i), selected: st.selected == Some(i), nav: matches!(s.kind, StepKind::Undo | StepKind::Redo | StepKind::GoTo(_)) }
    }).collect();
    if let Some(mut list) = desk_ref.widget(cx, ids!(h_list)).borrow_mut::<HistoryList>() {
        list.set_rows(cx, rows);
    }
}

pub fn handle_actions(page: &mut VideoCut, cx: &mut Cx, ui: &WidgetRef, actions: &Actions) {
    let desk_ref = ui.widget(cx, ids!(cut_motion));
    let list = desk_ref.widget(cx, ids!(h_list));
    for action in actions.filter_widget_actions_cast::<HistoryAction>(list.widget_uid()) {
        match action {
            HistoryAction::Go(n) => {
                go_to(page, cx, n);
            }
            HistoryAction::Hover(n) => {
                // The code it touched, shown (when the document is in that state).
                let Some(session) = page.motion.desk.session.as_ref() else { continue };
                let range = n.and_then(|n| session.history.get(n)).filter(|s| s.after.source == session.source).and_then(|s| s.range.clone());
                if let (Some(range), Some(mut code)) = (range, desk_ref.widget(cx, ids!(m_code)).borrow_mut::<super::code_timeline::MotionCodeTimeline>()) {
                    code.reveal(cx, 0, range.start);
                }
            }
            HistoryAction::None => {}
        }
    }
}

/// The document to the state after entry `n` (a new entry; Cmd-Z returns).
pub fn go_to(page: &mut VideoCut, cx: &mut Cx, n: usize) -> bool {
    page.motion.history.selected = Some(n);
    page.motion.history.shown = None;
    // The agent's step `picked` ("from here").
    if let Some(step) = page.motion.desk.session.as_ref().and_then(|s| s.history.get(n)) {
        super::history::pick(Some((step.label.clone(), step.after.source.clone(), step.after.modules.clone())));
    }
    super::desk::flush_and_settle(page);
    let Some(session) = page.motion.desk.session.as_mut() else { return false };
    if !session.go_to(n) {
        return false;
    }
    page.motion.desk.author = Some("You · history".into());
    super::desk::changed(page);
    page.motion.desk.repaint = true;
    cx.new_next_frame();
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_history_round_trips_through_its_file() {
        let steps = vec![
            HistoryStep { label: "Opened".into(), source: String::new(), time: 1.0, kind: StepKind::Opened, after: Snapshot { source: "a".into(), modules: vec![("m@1".into(), "x".into())] }, range: None },
            HistoryStep { label: "size 1 → 2".into(), source: "You · panel".into(), time: 2.5, kind: StepKind::Edit, after: Snapshot { source: "b".into(), modules: vec![("m@1".into(), "x".into())] }, range: Some(3..4) },
            HistoryStep { label: "Back to: Opened".into(), source: String::new(), time: 3.0, kind: StepKind::GoTo(0), after: Snapshot { source: "a".into(), modules: vec![("m@1".into(), "x".into())] }, range: None },
        ];
        let text = to_json(&steps);
        assert_eq!(text.matches("\"a\"").count(), 1, "{text}");
        assert_eq!(from_json(&text).unwrap(), steps);
    }
}

//! What a run records besides its frames (EDITS.md §6.4 step 6, §7.1):
//! marks, per-call frame ranges, failures and the focus track, all kept on
//! the virtual clock and mapped to the recording's frame numbers when the
//! run ends.
//!
//! Clock frames count every `/step` frame since the capture started. The
//! recording writes the frames of its segments (`cut()` ends one,
//! `resume()` opens the next), so output frame `n` is the `n`-th frame of
//! the segments. An input applied at clock frame `c` first shows in frame
//! `c`, which is what a mark or an event starting then refers to.

use makepad_strict_json::{self as json, Value};

/// A rectangle in source (recording) pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn union(self, other: Rect) -> Rect {
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = (self.x + self.w).max(other.x + other.w);
        let y1 = (self.y + self.h).max(other.y + other.h);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    /// The bounding box of points (a cursor path), zero-sized for one.
    pub fn around(points: &[(f64, f64)]) -> Option<Rect> {
        let first = points.first()?;
        Some(points.iter().fold(Rect::new(first.0, first.1, 0.0, 0.0), |r, p| r.union(Rect::new(p.0, p.1, 0.0, 0.0))))
    }

    pub fn grow(self, pad: f64) -> Rect {
        Rect::new(self.x - pad, self.y - pad, self.w + 2.0 * pad, self.h + 2.0 * pad)
    }

    fn to_json(self) -> Value {
        Value::Arr(vec![num(self.x), num(self.y), num(self.w), num(self.h)])
    }
}

/// Rounded to 1/1000: pixel positions and weights need no more, and the
/// file compares equal between identical runs.
pub(crate) fn num(v: f64) -> Value {
    let r = (v * 1000.0).round() / 1000.0;
    if r.fract() == 0.0 && r.abs() < 9e15 {
        Value::Int(r as i64)
    } else {
        Value::F64(r)
    }
}

/// Where a focus rect came from (§7.1's table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocusSource {
    Click,
    Drag,
    Scroll,
    Drop,
    Type,
    Wait,
    Focus,
    Wide,
    Layout,
}

impl FocusSource {
    pub fn name(self) -> &'static str {
        match self {
            FocusSource::Click => "click",
            FocusSource::Drag => "drag",
            FocusSource::Scroll => "scroll",
            FocusSource::Drop => "drop",
            FocusSource::Type => "type",
            FocusSource::Wait => "wait",
            FocusSource::Focus => "focus",
            FocusSource::Wide => "wide",
            FocusSource::Layout => "layout",
        }
    }

    /// `focus`/`focus_rect` beat the automatic sources while active.
    fn explicit(self) -> bool {
        matches!(self, FocusSource::Focus)
    }
}

/// One focus rect, active on clock frames `[from, to)`.
#[derive(Clone, Debug, PartialEq)]
pub struct FocusEvent {
    pub source: FocusSource,
    pub rect: Rect,
    pub weight: f64,
    pub from: u64,
    pub to: u64,
}

/// A script call and the clock frames it took, `[from, to)`.
#[derive(Clone, Debug, PartialEq)]
pub struct CallRecord {
    pub call: String,
    pub target: Option<String>,
    pub from: u64,
    pub to: u64,
    /// It used window coordinates instead of a widget path (§6.2: allowed,
    /// flagged in review).
    pub coordinates: bool,
    /// The SHA-256 of a dropped file.
    pub file_sha256: Option<String>,
    /// Frames of the call spent waiting rather than on the script's
    /// schedule (a `wait_*`, the multi-click spacing before a press).
    pub waited: u64,
    /// The text a `type` call typed.
    pub text: Option<String>,
}

/// A pointer press: down on clock frame `down`, up on `up`, at a point in
/// source pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Press {
    pub down: u64,
    pub up: Option<u64>,
    pub at: (f64, f64),
    pub button: u8,
}

/// Keyboard input as the app received it: a typed character, or a key
/// with its modifiers (down on the input's clock frame, up on `up`).
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Text(String),
    Key { code: String, mods: Vec<String>, up: u64 },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub call: String,
    pub frame: u64,
    pub message: String,
}

/// A recorded stretch of the clock: frames `[start, end)` (end open while
/// recording) are output frames `out_start..`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    start: u64,
    end: Option<u64>,
    out_start: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Track {
    segments: Vec<Segment>,
    pub events: Vec<FocusEvent>,
    marks: Vec<(String, u64)>,
    pub calls: Vec<CallRecord>,
    pub failures: Vec<Failure>,
    /// The clock frame now: every frame stepped so far.
    pub clock: u64,
    /// The pointer in source pixels, from the clock frame it got there
    /// (one entry per change).
    pointer: Vec<(u64, (f64, f64))>,
    pub presses: Vec<Press>,
    pub inputs: Vec<(u64, Input)>,
    /// Every listed widget's rect in source pixels from the clock frame it
    /// was listed with it, `None` from the frame it was no longer listed
    /// (one entry per change), by layout key (path or id and type, `#n`).
    widgets: Vec<(String, Vec<(u64, Option<Rect>)>)>,
}

impl Track {
    /// A track recording from clock frame 0.
    pub fn new() -> Track {
        Track { segments: vec![Segment { start: 0, end: None, out_start: 0 }], ..Default::default() }
    }

    pub fn recording(&self) -> bool {
        self.segments.last().is_some_and(|s| s.end.is_none())
    }

    pub fn advance(&mut self, frames: u64) {
        self.clock += frames;
    }

    /// `cut()`: the frames from now on are not written. False when already cut.
    pub fn cut(&mut self) -> bool {
        let clock = self.clock;
        match self.segments.last_mut() {
            Some(segment) if segment.end.is_none() => {
                segment.end = Some(clock);
                true
            }
            _ => false,
        }
    }

    /// `resume()`: writing again from now. False when not cut.
    pub fn resume(&mut self) -> bool {
        if self.recording() {
            return false;
        }
        let out_start = self.frames_written();
        self.segments.push(Segment { start: self.clock, end: None, out_start });
        true
    }

    /// Output frames so far.
    pub fn frames_written(&self) -> u64 {
        self.segments.iter().map(|s| s.end.unwrap_or(self.clock) - s.start).sum()
    }

    /// The output frame showing clock frame `clock`, or while cut the next
    /// frame written (a mark in a cut lands on the frame the recording
    /// resumes with).
    pub fn out_frame(&self, clock: u64) -> u64 {
        for s in &self.segments {
            let end = s.end.unwrap_or(u64::MAX);
            if clock < s.start {
                return s.out_start;
            }
            if clock < end {
                return s.out_start + clock - s.start;
            }
        }
        self.frames_written()
    }

    /// Output frames `[a, b)` of clock frames `[from, to)`: the recorded
    /// frames inside, empty when all of it was cut.
    fn out_range(&self, from: u64, to: u64) -> Option<(u64, u64)> {
        let mut range: Option<(u64, u64)> = None;
        for s in &self.segments {
            let end = s.end.unwrap_or(self.clock.max(to));
            let (a, b) = (from.max(s.start), to.min(end));
            if a < b {
                let (oa, ob) = (s.out_start + a - s.start, s.out_start + b - s.start);
                range = Some(range.map_or((oa, ob), |r| (r.0, ob)));
            }
        }
        range
    }

    /// Name the frame the next step shows: the index of the first frame
    /// with the state the calls so far left (a mark after the last call is
    /// `frames`, one past the last frame: the clip's end). False when the
    /// name is taken.
    pub fn mark(&mut self, name: &str) -> bool {
        if self.marks.iter().any(|(n, _)| n == name) {
            return false;
        }
        self.marks.push((name.to_string(), self.clock));
        true
    }

    pub fn marks(&self) -> Vec<(String, u64)> {
        self.marks.iter().map(|(n, c)| (n.clone(), self.out_frame(*c))).collect()
    }

    pub fn focus(&mut self, source: FocusSource, rect: Rect, weight: f64, from: u64, to: u64) {
        if to > from && rect.w >= 0.0 && rect.h >= 0.0 {
            self.events.push(FocusEvent { source, rect, weight, from, to });
        }
    }

    /// The focus rects active on clock frame `clock`, after the precedence
    /// rules: an active `wide` (the newest) overrides everything, active
    /// explicit `focus` rects beat the automatic ones.
    pub fn active(&self, clock: u64) -> Vec<&FocusEvent> {
        let live: Vec<&FocusEvent> = self.events.iter().filter(|e| e.from <= clock && clock < e.to).collect();
        if let Some(wide) = live.iter().rev().find(|e| e.source == FocusSource::Wide) {
            return vec![*wide];
        }
        if live.iter().any(|e| e.source.explicit()) {
            return live.into_iter().filter(|e| e.source.explicit()).collect();
        }
        live
    }

    /// The pointer is at `at` (source pixels) from clock frame `clock`.
    pub fn pointer(&mut self, clock: u64, at: (f64, f64)) {
        match self.pointer.last_mut() {
            Some(last) if last.1 == at => {}
            Some(last) if last.0 == clock => last.1 = at,
            _ => self.pointer.push((clock, at)),
        }
    }

    /// A widget listing (rects in source pixels) taken at clock frame
    /// `clock`: rects that changed, widgets that appeared and widgets that
    /// went are recorded from then.
    pub fn listing(&mut self, clock: u64, now: &[SnapWidget]) {
        for (key, line) in self.widgets.iter_mut() {
            if line.last().is_some_and(|(_, r)| r.is_some()) && !now.iter().any(|w| &w.key == key) {
                line.push((clock, None));
            }
        }
        for w in now {
            match self.widgets.iter_mut().find(|(key, _)| *key == w.key) {
                Some((_, line)) => {
                    if line.last().map(|(_, r)| *r) != Some(Some(w.rect)) {
                        line.push((clock, Some(w.rect)));
                    }
                }
                None => self.widgets.push((w.key.clone(), vec![(clock, Some(w.rect))])),
            }
        }
    }

    /// What an overlay binds to (reports/M4-demo-overlays.md): the pointer
    /// path `[[frame, x, y]]` and its presses, keyboard input, and every
    /// listed widget's rect over time `{key: [[frame, x, y, w, h] | [frame]]}`
    /// (`[frame]`: no longer listed), all on output frames. Clock frames in
    /// a cut land on the frame the recording resumes with; of several
    /// entries on one output frame the last is kept.
    pub fn bind_json(&self) -> Value {
        fn last_per_frame(entries: Vec<(u64, Value)>) -> Vec<Value> {
            let mut out: Vec<(u64, Value)> = Vec::new();
            for (frame, v) in entries {
                match out.last_mut() {
                    Some(last) if last.0 == frame => last.1 = v,
                    _ => out.push((frame, v)),
                }
            }
            out.into_iter().map(|(_, v)| v).collect()
        }
        let frame = |clock: u64| Value::Int(self.out_frame(clock) as i64);
        let path = last_per_frame(
            self.pointer.iter().map(|(c, p)| (self.out_frame(*c), Value::Arr(vec![frame(*c), num(p.0), num(p.1)]))).collect(),
        );
        let presses = self
            .presses
            .iter()
            .map(|p| {
                let up = p.up.unwrap_or(self.clock);
                json::obj(vec![
                    ("frames", Value::Arr(vec![frame(p.down), frame(up)])),
                    ("at", Value::Arr(vec![num(p.at.0), num(p.at.1)])),
                    ("button", Value::Int(p.button as i64)),
                ])
            })
            .collect();
        let keys = self
            .inputs
            .iter()
            .map(|(c, input)| match input {
                Input::Text(t) => json::obj(vec![("frame", frame(*c)), ("text", json::s(t))]),
                Input::Key { code, mods, up } => json::obj(vec![
                    ("frame", frame(*c)),
                    ("key", json::s(code)),
                    ("mods", Value::Arr(mods.iter().map(json::s).collect())),
                    ("up", frame(*up)),
                ]),
            })
            .collect();
        let widgets = self
            .widgets
            .iter()
            .map(|(key, line)| {
                let entries = line
                    .iter()
                    .map(|(c, r)| {
                        let mut v = vec![frame(*c)];
                        if let Some(r) = r {
                            v.extend([num(r.x), num(r.y), num(r.w), num(r.h)]);
                        }
                        (self.out_frame(*c), Value::Arr(v))
                    })
                    .collect();
                (key.clone(), Value::Arr(last_per_frame(entries)))
            })
            .collect();
        json::obj(vec![
            ("pointer", json::obj(vec![("path", Value::Arr(path)), ("presses", Value::Arr(presses))])),
            ("keys", Value::Arr(keys)),
            ("widgets", Value::Obj(widgets)),
        ])
    }

    /// The focus track as run.json stores it: `events` (each rect with its
    /// output frame range) and `frames`, the active rects of every output
    /// frame run-length encoded as `{from, to, rects: [[x, y, w, h,
    /// weight, source]]}` (frames without a rect are left out).
    pub fn focus_json(&self) -> Value {
        let events = self
            .events
            .iter()
            .filter_map(|e| {
                let (a, b) = self.out_range(e.from, e.to)?;
                Some(json::obj(vec![
                    ("source", json::s(e.source.name())),
                    ("rect", e.rect.to_json()),
                    ("weight", num(e.weight)),
                    ("frames", Value::Arr(vec![Value::Int(a as i64), Value::Int(b as i64)])),
                ]))
            })
            .collect();
        let mut runs: Vec<(u64, u64, Value)> = Vec::new();
        for s in &self.segments {
            let end = s.end.unwrap_or(self.clock);
            for clock in s.start..end {
                let rects = Value::Arr(
                    self.active(clock)
                        .into_iter()
                        .map(|e| {
                            let mut r = match e.rect.to_json() {
                                Value::Arr(r) => r,
                                _ => unreachable!(),
                            };
                            r.push(num(e.weight));
                            r.push(json::s(e.source.name()));
                            Value::Arr(r)
                        })
                        .collect(),
                );
                let out = s.out_start + clock - s.start;
                match runs.last_mut() {
                    Some(run) if run.1 == out && run.2 == rects => run.1 = out + 1,
                    _ => runs.push((out, out + 1, rects)),
                }
            }
        }
        let frames = runs
            .into_iter()
            .filter(|run| run.2.as_arr().is_some_and(|a| !a.is_empty()))
            .map(|(a, b, rects)| json::obj(vec![("from", Value::Int(a as i64)), ("to", Value::Int(b as i64)), ("rects", rects)]))
            .collect();
        json::obj(vec![("events", Value::Arr(events)), ("frames", Value::Arr(frames))])
    }

    pub fn calls_json(&self) -> Value {
        Value::Arr(
            self.calls
                .iter()
                .map(|c| {
                    let frames = match self.out_range(c.from, c.to.max(c.from + 1)) {
                        Some((a, b)) if c.to > c.from => Value::Arr(vec![Value::Int(a as i64), Value::Int(b as i64)]),
                        _ => {
                            let at = self.out_frame(c.from) as i64;
                            Value::Arr(vec![Value::Int(at), Value::Int(at)])
                        }
                    };
                    let mut fields = vec![
                        ("call", json::s(&c.call)),
                        ("clock", Value::Arr(vec![Value::Int(c.from as i64), Value::Int(c.to as i64)])),
                        ("frames", frames),
                    ];
                    if let Some(target) = &c.target {
                        fields.push(("target", json::s(target)));
                    }
                    if c.coordinates {
                        fields.push(("coordinates", Value::Bool(true)));
                    }
                    if c.waited > 0 {
                        fields.push(("waited", Value::Int(c.waited as i64)));
                    }
                    if let Some(sha) = &c.file_sha256 {
                        fields.push(("file_sha256", json::s(sha)));
                    }
                    if let Some(text) = &c.text {
                        fields.push(("text", json::s(text)));
                    }
                    json::obj(fields)
                })
                .collect(),
        )
    }

    pub fn marks_json(&self) -> Value {
        Value::Obj(self.marks().into_iter().map(|(n, f)| (n, Value::Int(f as i64))).collect())
    }

    pub fn failures_json(&self) -> Value {
        Value::Arr(
            self.failures
                .iter()
                .map(|f| {
                    json::obj(vec![
                        ("call", json::s(&f.call)),
                        ("frame", Value::Int(self.out_frame(f.frame) as i64)),
                        ("message", json::s(&f.message)),
                    ])
                })
                .collect(),
        )
    }

    /// The recorded segments as clock-frame ranges `[[start, end]]`; output
    /// frames number the segments' frames in order.
    pub fn segments_json(&self) -> Value {
        Value::Arr(
            self.segments
                .iter()
                .map(|s| Value::Arr(vec![Value::Int(s.start as i64), Value::Int(s.end.unwrap_or(self.clock) as i64)]))
                .collect(),
        )
    }
}

/// A node of the semantic tree: its path (or id and type when the app
/// registers no paths) and its rect in source pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapWidget {
    pub key: String,
    pub rect: Rect,
}

/// The region a layout change covers between two listings: every widget
/// that appeared, disappeared or moved, old and new rects together. The
/// window's own row is ignored (its size is the output).
pub fn layout_change(before: &[SnapWidget], after: &[SnapWidget]) -> Option<Rect> {
    let mut region: Option<Rect> = None;
    let mut add = |r: Rect| region = Some(region.map_or(r, |g| g.union(r)));
    for b in before {
        match after.iter().find(|a| a.key == b.key) {
            Some(a) if a.rect == b.rect => {}
            Some(a) => {
                add(a.rect);
                add(b.rect);
            }
            None => add(b.rect),
        }
    }
    for a in after {
        if !before.iter().any(|b| b.key == a.key) {
            add(a.rect);
        }
    }
    region.filter(|r| r.w > 0.0 && r.h > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_and_calls_map_onto_the_written_frames() {
        let mut t = Track::new();
        t.advance(10);
        t.mark("a");
        assert!(t.cut());
        assert!(!t.cut());
        t.advance(5);
        t.mark("in_cut");
        assert!(t.resume());
        t.advance(4);
        t.mark("b");
        assert_eq!(t.frames_written(), 14);
        let marks = t.marks();
        assert_eq!(marks, vec![("a".to_string(), 10), ("in_cut".to_string(), 10), ("b".to_string(), 14)]);
        assert_eq!(t.out_range(8, 17), Some((8, 12)));
        assert_eq!(t.out_range(11, 14), None);
        assert_eq!(t.out_frame(16), 11);
    }

    #[test]
    fn wide_overrides_and_explicit_focus_beats_automatic() {
        let mut t = Track::new();
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        t.focus(FocusSource::Click, r, 1.0, 0, 30);
        t.focus(FocusSource::Focus, r.grow(5.0), 0.7, 10, 20);
        t.focus(FocusSource::Wide, Rect::new(0.0, 0.0, 100.0, 100.0), 1.0, 15, 18);
        assert_eq!(t.active(5).len(), 1);
        assert_eq!(t.active(12)[0].source, FocusSource::Focus);
        assert_eq!(t.active(16)[0].source, FocusSource::Wide);
        assert_eq!(t.active(25)[0].source, FocusSource::Click);
        assert!(t.active(30).is_empty());
    }

    #[test]
    fn the_per_frame_track_is_run_length_encoded() {
        let mut t = Track::new();
        t.focus(FocusSource::Click, Rect::new(1.0, 2.0, 3.0, 4.0), 1.0, 2, 6);
        t.focus(FocusSource::Layout, Rect::new(0.0, 0.0, 5.0, 5.0), 0.6, 4, 8);
        t.advance(10);
        let focus = t.focus_json();
        let frames = focus.get("frames").unwrap().as_arr().unwrap();
        let spans: Vec<(i64, i64, usize)> = frames
            .iter()
            .map(|f| (f.get("from").unwrap().as_i64().unwrap(), f.get("to").unwrap().as_i64().unwrap(), f.get("rects").unwrap().as_arr().unwrap().len()))
            .collect();
        assert_eq!(spans, vec![(2, 4, 1), (4, 6, 2), (6, 8, 1)]);
        assert_eq!(focus.get("events").unwrap().as_arr().unwrap().len(), 2);
    }

    #[test]
    fn pointer_input_and_widgets_are_recorded_on_output_frames() {
        let mut t = Track::new();
        t.pointer(0, (10.0, 10.0));
        t.pointer(0, (12.0, 10.0));
        t.pointer(3, (12.0, 10.0));
        t.pointer(4, (20.0, 30.0));
        t.presses.push(Press { down: 4, up: Some(7), at: (20.0, 30.0), button: 0 });
        t.inputs.push((8, Input::Text("a".into())));
        t.inputs.push((9, Input::Key { code: "KeyS".into(), mods: vec!["cmd".into()], up: 12 }));
        let w = |k: &str, x: f64| SnapWidget { key: k.into(), rect: Rect::new(x, 0.0, 10.0, 10.0) };
        t.listing(0, &[w("a#0", 0.0), w("b#0", 5.0)]);
        t.listing(6, &[w("a#0", 0.0), w("b#0", 9.0)]);
        t.listing(12, &[w("b#0", 9.0)]);
        t.listing(14, &[w("a#0", 1.0), w("b#0", 9.0)]);
        t.advance(20);
        let b = t.bind_json();
        assert_eq!(b.get("pointer").unwrap().get("path").unwrap().to_json(), "[[0,12,10],[4,20,30]]");
        assert_eq!(b.get("pointer").unwrap().get("presses").unwrap().to_json(), r#"[{"frames":[4,7],"at":[20,30],"button":0}]"#);
        assert_eq!(b.get("keys").unwrap().as_arr().unwrap().len(), 2);
        assert_eq!(b.get("widgets").unwrap().get("a#0").unwrap().to_json(), "[[0,0,0,10,10],[12],[14,1,0,10,10]]");
        assert_eq!(b.get("widgets").unwrap().get("b#0").unwrap().to_json(), "[[0,5,0,10,10],[6,9,0,10,10]]");
    }

    #[test]
    fn a_layout_change_covers_what_appeared_moved_or_went() {
        let w = |k: &str, x: f64| SnapWidget { key: k.into(), rect: Rect::new(x, 0.0, 10.0, 10.0) };
        assert_eq!(layout_change(&[w("a", 0.0)], &[w("a", 0.0)]), None);
        assert_eq!(layout_change(&[w("a", 0.0)], &[w("a", 0.0), w("dialog", 50.0)]), Some(Rect::new(50.0, 0.0, 10.0, 10.0)));
        assert_eq!(layout_change(&[w("a", 0.0), w("b", 20.0)], &[w("a", 5.0)]), Some(Rect::new(0.0, 0.0, 30.0, 10.0)));
    }
}

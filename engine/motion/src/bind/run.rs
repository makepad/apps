//! An app run's recorded data (EDITS.md §6.4 step 6, §7.1), read from its
//! `run.json`, as functions of the run's own time: where the action is (the
//! focus track), where a widget is, where the pointer is and when it
//! pressed, what was typed, and the marks.
//!
//! Every query takes seconds of the recording (`frame / fps`) and is pure:
//! the same file answers the same at any time, in any order, so a bound
//! overlay is a pure function of time like the rest of a motion document.
//! All rectangles and points are in source pixels (the recording's pixel
//! grid, origin top left).
//!
//! run.json written before the `bind` section existed (pointer, keys,
//! widget rects over time) still loads: widget rects then come from the
//! focus events that name them exactly (`type`, `wait`), presses from the
//! click calls, and `RunData::derived` says so.

use makepad_strict_json::{self as json, Value};

/// A rectangle in source pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }

    pub fn centre(&self) -> (f64, f64) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Rect::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }

    pub fn grow(&self, pad: f64) -> Rect {
        Rect::new(self.x - pad, self.y - pad, self.w + 2.0 * pad, self.h + 2.0 * pad)
    }

    pub fn lerp(&self, to: &Rect, u: f64) -> Rect {
        let l = |a: f64, b: f64| a + (b - a) * u;
        Rect::new(l(self.x, to.x), l(self.y, to.y), l(self.w, to.w), l(self.h, to.h))
    }

    pub fn is_empty(&self) -> bool {
        !(self.w > 0.0 && self.h > 0.0)
    }

    fn from_json(v: &[Value]) -> Option<Rect> {
        match v {
            [x, y, w, h, ..] => Some(Rect::new(f(x)?, f(y)?, f(w)?, f(h)?)),
            _ => None,
        }
    }
}

fn f(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::F64(x) => Some(*x),
        _ => None,
    }
}

fn frame(v: &Value) -> Option<u64> {
    f(v).filter(|x| *x >= 0.0).map(|x| x as u64)
}

/// A script call and the output frames it took, `[from, to)`.
#[derive(Clone, Debug, PartialEq)]
pub struct RunCall {
    pub call: String,
    pub target: Option<String>,
    pub from: u64,
    pub to: u64,
    /// What a `type` call typed (runs recorded with the `bind` section).
    pub text: Option<String>,
}

/// A pointer press, down on frame `down` and up on `up`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RunPress {
    pub down: u64,
    pub up: u64,
    pub at: (f64, f64),
    pub button: u8,
}

/// Keyboard input: a typed character, or a key with modifiers held from
/// `frame` to `up`.
#[derive(Clone, Debug, PartialEq)]
pub enum RunKey {
    Text { frame: u64, text: String },
    Key { frame: u64, up: u64, code: String, mods: Vec<String> },
}

impl RunKey {
    pub fn frame(&self) -> u64 {
        match self {
            RunKey::Text { frame, .. } | RunKey::Key { frame, .. } => *frame,
        }
    }

    /// A shortcut worth a badge: a key with a modifier, or a named key
    /// that is not plain typing (Escape, arrows); Return and Tab while
    /// typing are part of the text.
    pub fn is_shortcut(&self) -> bool {
        match self {
            RunKey::Key { mods, code, .. } => !mods.is_empty() || !matches!(code.as_str(), "ReturnKey" | "Tab"),
            RunKey::Text { .. } => false,
        }
    }

    /// How a badge shows it: `⌘ S`, `⇧ ⌘ Z`, `Esc`, `a`.
    pub fn label(&self) -> String {
        match self {
            RunKey::Text { text, .. } => text.clone(),
            RunKey::Key { code, mods, .. } => {
                let mut parts: Vec<&str> = Vec::new();
                // The macOS order: control, option, shift, command.
                for (m, sym) in [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("cmd", "⌘")] {
                    if mods.iter().any(|x| x == m) {
                        parts.push(sym);
                    }
                }
                let key = key_name(code);
                let mut out = parts.join(" ");
                if !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&key);
                out
            }
        }
    }
}

/// A key code as a person reads it: `KeyS` → `S`, `ReturnKey` → `↩`.
pub fn key_name(code: &str) -> String {
    if let Some(k) = code.strip_prefix("Key").filter(|k| k.len() == 1) {
        return k.to_uppercase();
    }
    if let Some(d) = code.strip_prefix("Digit").filter(|k| k.len() == 1) {
        return d.to_string();
    }
    match code {
        "ReturnKey" | "Return" | "Enter" => "↩",
        "Tab" => "⇥",
        "Escape" => "Esc",
        "Backspace" => "⌫",
        "Delete" => "⌦",
        "Space" => "Space",
        "ArrowUp" => "↑",
        "ArrowDown" => "↓",
        "ArrowLeft" => "←",
        "ArrowRight" => "→",
        other => other,
    }
    .to_string()
}

/// The focus rects active on output frames `[from, to)`: rect, weight and
/// source (§7.1).
#[derive(Clone, Debug, PartialEq)]
pub struct FocusSpan {
    pub from: u64,
    pub to: u64,
    pub rects: Vec<(Rect, f64, String)>,
}

/// A `type` call in progress at some time.
#[derive(Clone, Debug, PartialEq)]
pub struct Typing {
    pub target: Option<String>,
    /// The field's rect then, when known.
    pub rect: Option<Rect>,
    /// The text typed so far (runs recorded with the `bind` section; empty
    /// otherwise).
    pub text: String,
    /// 0 at the first character, 1 at the call's end.
    pub progress: f64,
    /// Seconds since the call started and until it ends.
    pub age: f64,
    pub left: f64,
}

/// Where the story is at some time: the last mark reached, 1-based.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub index: usize,
    pub count: usize,
    pub name: String,
    /// Seconds since the mark.
    pub age: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunData {
    pub label: String,
    pub fps: f64,
    pub frames: u64,
    /// The recording's size in pixels.
    pub size: (f64, f64),
    /// Mark name and output frame, in frame order.
    pub marks: Vec<(String, u64)>,
    pub calls: Vec<RunCall>,
    pub focus: Vec<FocusSpan>,
    /// The pointer from the frame it got somewhere (one entry per change).
    pub pointer: Vec<(u64, (f64, f64))>,
    pub presses: Vec<RunPress>,
    pub keys: Vec<RunKey>,
    /// Layout key (path or `id:Type`, with `#n` for repeats) and its rect
    /// from a frame on, `None` from the frame it stopped being listed.
    pub widgets: Vec<(String, Vec<(u64, Option<Rect>)>)>,
    /// No `bind` section: widgets and presses were derived from the focus
    /// events and calls, and pointer paths and keys are unknown.
    pub derived: bool,
}

/// Weights under this fraction of the strongest active one are left out
/// of the focus union (explicit focus beats automatic: the reframing
/// solver's rule, so an overlay and the camera agree).
pub const WEIGHT_FLOOR: f64 = 0.5;

impl RunData {
    pub fn parse(bytes: &[u8]) -> Result<RunData, String> {
        let v = json::parse_depth(bytes, 32).map_err(|e| format!("run.json: {e}"))?;
        RunData::from_value(&v)
    }

    pub fn from_value(v: &Value) -> Result<RunData, String> {
        let format = v.get("format").and_then(Value::as_str).unwrap_or("");
        if !format.starts_with("makepad-app-run/") {
            return Err(format!("run.json: format {format:?} is not an app run's (makepad-app-run/1)"));
        }
        let output = v.get("output").ok_or("run.json: no output")?;
        let fps = output.get("fps").and_then(f).filter(|x| *x > 0.0).ok_or("run.json: output.fps missing")?;
        let size = (
            output.get("width").and_then(f).ok_or("run.json: output.width missing")?,
            output.get("height").and_then(f).ok_or("run.json: output.height missing")?,
        );
        let frames = v.get("frames").and_then(frame).ok_or("run.json: frames missing")?;
        let mut marks: Vec<(String, u64)> = match v.get("marks") {
            Some(Value::Obj(m)) => m.iter().filter_map(|(k, x)| Some((k.clone(), frame(x)?))).collect(),
            _ => Vec::new(),
        };
        marks.sort_by_key(|m| m.1);
        let pair = |x: Option<&Value>| -> Option<(u64, u64)> {
            let a = x?.as_arr()?;
            Some((frame(a.first()?)?, frame(a.get(1)?)?))
        };
        let calls = v
            .get("calls")
            .and_then(Value::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|c| {
                let (from, to) = pair(c.get("frames"))?;
                Some(RunCall {
                    call: c.get("call")?.as_str()?.to_string(),
                    target: c.get("target").and_then(Value::as_str).map(str::to_string),
                    from,
                    to,
                    text: c.get("text").and_then(Value::as_str).map(str::to_string),
                })
            })
            .collect::<Vec<_>>();
        let focus = v
            .get("focus")
            .and_then(|x| x.get("frames"))
            .and_then(Value::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|s| {
                let rects = s
                    .get("rects")?
                    .as_arr()?
                    .iter()
                    .filter_map(|r| {
                        let r = r.as_arr()?;
                        let rect = Rect::from_json(r)?;
                        Some((rect, r.get(4).and_then(f).unwrap_or(1.0), r.get(5).and_then(Value::as_str).unwrap_or("").to_string()))
                    })
                    .collect();
                Some(FocusSpan { from: frame(s.get("from")?)?, to: frame(s.get("to")?)?, rects })
            })
            .collect();
        let mut run = RunData {
            label: v.get("label").and_then(Value::as_str).unwrap_or("").to_string(),
            fps,
            frames,
            size,
            marks,
            calls,
            focus,
            ..Default::default()
        };
        match v.get("bind") {
            Some(bind) => run.read_bind(bind),
            None => run.derive(v),
        }
        Ok(run)
    }

    fn read_bind(&mut self, bind: &Value) {
        let pointer = bind.get("pointer");
        self.pointer = pointer
            .and_then(|p| p.get("path"))
            .and_then(Value::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|e| {
                let e = e.as_arr()?;
                Some((frame(e.first()?)?, (f(e.get(1)?)?, f(e.get(2)?)?)))
            })
            .collect();
        self.presses = pointer
            .and_then(|p| p.get("presses"))
            .and_then(Value::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|p| {
                let frames = p.get("frames")?.as_arr()?;
                let at = p.get("at")?.as_arr()?;
                Some(RunPress {
                    down: frame(frames.first()?)?,
                    up: frame(frames.get(1)?)?,
                    at: (f(at.first()?)?, f(at.get(1)?)?),
                    button: p.get("button").and_then(frame).unwrap_or(0) as u8,
                })
            })
            .collect();
        self.keys = bind
            .get("keys")
            .and_then(Value::as_arr)
            .unwrap_or(&[])
            .iter()
            .filter_map(|k| {
                let at = frame(k.get("frame")?)?;
                if let Some(text) = k.get("text").and_then(Value::as_str) {
                    return Some(RunKey::Text { frame: at, text: text.to_string() });
                }
                Some(RunKey::Key {
                    frame: at,
                    up: k.get("up").and_then(frame).unwrap_or(at),
                    code: k.get("key")?.as_str()?.to_string(),
                    mods: k.get("mods").and_then(Value::as_arr).unwrap_or(&[]).iter().filter_map(|m| m.as_str().map(str::to_string)).collect(),
                })
            })
            .collect();
        if let Some(Value::Obj(widgets)) = bind.get("widgets") {
            self.widgets = widgets
                .iter()
                .map(|(key, line)| {
                    let line = line
                        .as_arr()
                        .unwrap_or(&[])
                        .iter()
                        .filter_map(|e| {
                            let e = e.as_arr()?;
                            Some((frame(e.first()?)?, Rect::from_json(&e[1..])))
                        })
                        .collect();
                    (key.clone(), line)
                })
                .collect();
        }
    }

    /// An older run.json: the focus events that are a widget's exact rect
    /// (`type`, `wait`) give widget rects from their frame, and each click
    /// call gives a press at its target's centre.
    fn derive(&mut self, v: &Value) {
        self.derived = true;
        let events = v.get("focus").and_then(|x| x.get("events")).and_then(Value::as_arr).unwrap_or(&[]);
        let mut found: Vec<(String, u64, Rect)> = Vec::new();
        for e in events {
            let source = e.get("source").and_then(Value::as_str).unwrap_or("");
            if source != "type" && source != "wait" {
                continue;
            }
            let (Some(rect), Some(frames)) = (e.get("rect").and_then(Value::as_arr).and_then(Rect::from_json), e.get("frames").and_then(Value::as_arr)) else {
                continue;
            };
            let Some(at) = frames.first().and_then(frame) else { continue };
            let call_name = if source == "type" { "type" } else { "wait_" };
            // The call the event belongs to: the one running at its frame.
            let call = self.calls.iter().rev().find(|c| c.call.starts_with(call_name) && c.from <= at && at <= c.to.max(c.from + 1));
            if let Some(target) = call.and_then(|c| c.target.clone()) {
                found.push((target, at, rect));
            }
        }
        found.sort_by_key(|x| x.1);
        for (target, at, rect) in found {
            let key = format!("{target}#0");
            match self.widgets.iter_mut().find(|(k, _)| *k == key) {
                Some((_, line)) => line.push((at, Some(rect))),
                None => self.widgets.push((key, vec![(at, Some(rect))])),
            }
        }
        // A widget known from later on was there before too (the best guess
        // without the listing): its first rect holds from frame 0.
        for (_, line) in self.widgets.iter_mut() {
            if let Some(first) = line.first_mut() {
                first.0 = 0;
            }
        }
        let press_frames = ((0.05 * self.fps).round() as u64).max(1);
        let settle = (self.fps / 60.0).round() as u64;
        let clicks: Vec<RunCall> = self.calls.iter().filter(|c| c.call == "click" || c.call == "type").cloned().collect();
        for c in clicks {
            let Some(target) = &c.target else { continue };
            // `type` clicks its field first: the press is 1/60 s + a press
            // after the move, which starts the call.
            let up = if c.call == "click" { c.to.saturating_sub(settle) } else { c.from + ((0.35 * self.fps).round() as u64) + press_frames };
            let down = up.saturating_sub(press_frames);
            let at = match self.widget(target, down as f64 / self.fps) {
                Some(r) => r.centre(),
                None => continue,
            };
            self.presses.push(RunPress { down, up, at, button: 0 });
        }
        self.presses.sort_by_key(|p| p.down);
    }

    /// The recording's length in seconds.
    pub fn duration(&self) -> f64 {
        self.frames as f64 / self.fps
    }

    /// The frame shown at `t` seconds (clamped into the recording).
    pub fn frame_at(&self, t: f64) -> u64 {
        let f = (t * self.fps + 1e-9).floor();
        if f <= 0.0 { 0 } else { (f as u64).min(self.frames.saturating_sub(1)) }
    }

    fn secs(&self, frame: u64) -> f64 {
        frame as f64 / self.fps
    }

    /// Every mark's time in seconds (what an edit's `marker()` cut points
    /// resolve against: `EditEval::set_marks`).
    pub fn mark_seconds(&self) -> Vec<(String, f64)> {
        self.marks.iter().map(|(n, f)| (n.clone(), self.secs(*f))).collect()
    }

    /// A mark's time in seconds.
    pub fn mark(&self, name: &str) -> Option<f64> {
        self.marks.iter().find(|m| m.0 == name).map(|m| self.secs(m.1))
    }

    /// The last mark reached at `t`, 1-based among `count`; `None` before
    /// the first mark.
    pub fn step(&self, t: f64) -> Option<Step> {
        let f = self.frame_at(t);
        let index = self.marks.iter().filter(|m| m.1 <= f).count();
        let (name, at) = self.marks.get(index.checked_sub(1)?)?;
        Some(Step { index, count: self.marks.len(), name: name.clone(), age: t - self.secs(*at) })
    }

    /// The raw focus target at `t`: the union of the active rects whose
    /// weight is at least `WEIGHT_FLOOR` of the strongest.
    pub fn focus_raw(&self, t: f64) -> Option<Rect> {
        let f = self.frame_at(t);
        let span = self.focus.iter().find(|s| s.from <= f && f < s.to)?;
        Self::target(span)
    }

    fn target(span: &FocusSpan) -> Option<Rect> {
        let strongest = span.rects.iter().map(|r| r.1).fold(0.0, f64::max);
        if strongest <= 0.0 {
            return None;
        }
        span.rects.iter().filter(|r| r.1 >= strongest * WEIGHT_FLOOR).map(|r| r.0).reduce(|a, b| a.union(&b))
    }

    /// The focus as a highlight follows it: each change eases (cubic out)
    /// from where the highlight was to the new target over `settle`
    /// seconds, and where nothing is in focus the last rect stays while
    /// its presence fades to 0 over `settle`. Returns the rect and its
    /// presence (0..1); `None` before the first focus.
    pub fn focus(&self, t: f64, settle: f64) -> Option<(Rect, f64)> {
        let settle = settle.max(1e-6);
        let f = self.frame_at(t);
        // The focus up to `f` as segments `(start, end, target)` in
        // seconds, gaps included (a gap has no target).
        let mut segments: Vec<(f64, f64, Option<Rect>)> = Vec::new();
        let mut end = 0;
        for span in self.focus.iter().filter(|s| s.from <= f) {
            if span.from > end {
                segments.push((self.secs(end), self.secs(span.from), None));
            }
            segments.push((self.secs(span.from), self.secs(span.to), Self::target(span)));
            end = span.to;
        }
        if end <= f {
            segments.push((self.secs(end), f64::INFINITY, None));
        }
        // A segment that lasted `settle` fixes the state at its end, so
        // only the ones after the last such segment matter.
        let begin = segments.iter().rposition(|s| s.1.min(t) - s.0 >= settle).unwrap_or(0);
        let mut state: Option<(Rect, f64)> = None;
        for &(start, end, target) in &segments[begin..] {
            let u = ((end.min(t) - start) / settle).clamp(0.0, 1.0);
            let eased = 1.0 - (1.0 - u).powi(3);
            state = match (state, target) {
                (None, None) => None,
                (None, Some(r)) => Some((r, eased)),
                (Some((r, p)), Some(target)) => Some((r.lerp(&target, eased), p + (1.0 - p) * eased)),
                (Some((r, p)), None) => Some((r, p * (1.0 - u))),
            };
        }
        state.filter(|(_, p)| *p > 0.0)
    }

    /// The layout keys `path` names: an exact key (`a.b#1`), else every key
    /// whose path is `path` or ends with it on a whole segment, first
    /// occurrence (`#0`) preferred.
    pub fn widget_keys(&self, path: &str) -> Vec<&str> {
        let path = path.trim_start_matches('@');
        if let Some(exact) = self.widgets.iter().find(|(k, _)| k == path) {
            return vec![exact.0.as_str()];
        }
        let mut keys: Vec<&str> = self
            .widgets
            .iter()
            .map(|(k, _)| k.as_str())
            .filter(|k| {
                let base = k.rsplit_once('#').map_or(*k, |(b, _)| b);
                base == path || (base.len() > path.len() && base.ends_with(path) && base.as_bytes()[base.len() - path.len() - 1] == b'.')
            })
            .collect();
        keys.sort_by_key(|k| !k.ends_with("#0"));
        keys
    }

    /// Every widget path recorded, for "did you mean".
    pub fn widget_paths(&self) -> Vec<&str> {
        self.widgets.iter().map(|(k, _)| k.rsplit_once('#').map_or(k.as_str(), |(b, _)| b)).collect()
    }

    /// A widget's rect at `t`, `None` while it is not listed (hidden,
    /// scrolled away, not yet open). Several widgets matching `path`: the
    /// first listed at `t`.
    pub fn widget(&self, path: &str, t: f64) -> Option<Rect> {
        let f = self.frame_at(t);
        self.widget_keys(path).into_iter().find_map(|key| {
            let line = &self.widgets.iter().find(|(k, _)| k == key)?.1;
            let i = line.partition_point(|e| e.0 <= f);
            line.get(i.checked_sub(1)?)?.1.filter(|r| !r.is_empty())
        })
    }

    /// The pointer at `t`, moving smoothly between the frames it was
    /// recorded on (a move is one position per frame).
    pub fn cursor(&self, t: f64) -> Option<(f64, f64)> {
        let x = t * self.fps;
        let i = self.pointer.partition_point(|e| (e.0 as f64) <= x);
        let a = self.pointer.get(i.checked_sub(1)?)?;
        match self.pointer.get(i) {
            // The next position is one frame on: part of the same move.
            Some(b) if b.0 == a.0 + 1 => {
                let u = (x - a.0 as f64).clamp(0.0, 1.0);
                Some((a.1 .0 + (b.1 .0 - a.1 .0) * u, a.1 .1 + (b.1 .1 - a.1 .1) * u))
            }
            _ => Some(a.1),
        }
    }

    /// The `nth` most recent press at or before `t` (0: the latest), with
    /// the seconds since it went down.
    pub fn press(&self, t: f64, nth: usize) -> Option<(RunPress, f64)> {
        let x = t * self.fps + 1e-9;
        let n = self.presses.partition_point(|p| p.down as f64 <= x);
        let p = self.presses.get(n.checked_sub(1 + nth)?)?;
        Some((*p, t - self.secs(p.down)))
    }

    /// The `type` call running at `t`, with the text typed so far.
    pub fn typing(&self, t: f64) -> Option<Typing> {
        let f = self.frame_at(t);
        let call = self.calls.iter().find(|c| c.call == "type" && c.from <= f && f < c.to)?;
        let chars: Vec<&RunKey> = self.keys.iter().filter(|k| k.frame() >= call.from && k.frame() < call.to).collect();
        let text: String = chars
            .iter()
            .filter(|k| k.frame() <= f)
            .map(|k| match k {
                RunKey::Text { text, .. } => text.as_str(),
                RunKey::Key { code, .. } if code == "ReturnKey" => "\n",
                RunKey::Key { code, .. } if code == "Tab" => "\t",
                _ => "",
            })
            .collect();
        let first = chars.first().map_or(call.from, |k| k.frame());
        let last = chars.last().map_or(call.to, |k| k.frame()).max(first + 1);
        Some(Typing {
            target: call.target.clone(),
            rect: call.target.as_deref().and_then(|p| self.widget(p, t)),
            text,
            progress: ((t * self.fps - first as f64) / (last - first) as f64).clamp(0.0, 1.0),
            age: t - self.secs(call.from),
            left: self.secs(call.to) - t,
        })
    }

    /// The latest shortcut pressed at or before `t` and within `hold`
    /// seconds of it, with its age.
    pub fn shortcut(&self, t: f64, hold: f64) -> Option<(&RunKey, f64)> {
        let x = t * self.fps + 1e-9;
        let k = self.keys.iter().rev().find(|k| k.is_shortcut() && k.frame() as f64 <= x)?;
        let age = t - self.secs(k.frame());
        (age < hold).then_some((k, age))
    }

    /// Every call span of one kind (`click`, `type`, …) as seconds
    /// `[from, to)`, in order.
    pub fn spans(&self, call: &str) -> Vec<(f64, f64)> {
        self.calls.iter().filter(|c| c.call == call).map(|c| (self.secs(c.from), self.secs(c.to))).collect()
    }

    /// The focus track in the form the reframing solver takes: per frame,
    /// the active rects and weights, and whether a `type` call runs.
    pub fn focus_frames(&self) -> Vec<(u64, Vec<(Rect, f64)>, bool)> {
        (0..self.frames)
            .map(|f| {
                let rects = self.focus.iter().find(|s| s.from <= f && f < s.to).map(|s| s.rects.iter().map(|r| (r.0, r.1)).collect()).unwrap_or_default();
                let typing = self.calls.iter().any(|c| c.call == "type" && c.from <= f && f < c.to);
                (f, rects, typing)
            })
            .collect()
    }
}

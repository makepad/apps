//! Bound values: what a motion document asks of its run (`run.focus().at`,
//! `run.widget(@save).size`, `run.click().age`, …) and their value at one
//! moment. A bound value is sampled, never integrated, so it is a pure
//! function of the moment like every other property.
//!
//! Geometry comes out in the graphic's coordinates (through the
//! [`BindFrame`]), times in seconds, and `presence` is 0..1: how much the
//! thing is there to show (a highlight fades with it).

use super::place::BindFrame;
use super::run::{Rect, RunData};

/// What is followed.
#[derive(Clone, Debug, PartialEq)]
pub enum Subject {
    /// Where the action is (§7.1's focus track), eased over `settle`
    /// seconds, padded by `pad` graphic units.
    Focus { pad: f64, settle: f64 },
    /// A widget by path, padded by `pad` graphic units.
    Widget { path: String, pad: f64 },
    /// The pointer.
    Cursor,
    /// The `nth` latest press (0: the latest); `progress` runs 0→1 over
    /// `dur` seconds from the press.
    Click { nth: usize, dur: f64 },
    /// The `type` call in progress (the field, the text so far).
    Typing { pad: f64 },
    /// The latest shortcut, shown for `hold` seconds.
    Key { hold: f64 },
    /// The last mark reached (a step counter).
    Step,
    /// Time relative to a mark: `age` is seconds since it (negative
    /// before), `presence` is 1 from the mark for `hold` seconds.
    Mark { name: String, hold: f64 },
}

/// Which part of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Field {
    /// Top-left corner, `vec2`.
    At,
    /// `vec2` width and height.
    Size,
    /// `vec2` centre.
    Centre,
    /// `[x, y, w, h]`.
    Rect,
    Presence,
    /// Seconds since it began.
    Age,
    /// 0..1 through it.
    Progress,
    /// Its text: the typed text, a key's label, "2 / 5" for a step.
    Text,
    /// A step's 1-based index.
    Index,
    /// How many steps there are.
    Count,
}

impl Field {
    pub fn from_name(name: &str) -> Option<Field> {
        Some(match name {
            "at" => Field::At,
            "size" => Field::Size,
            "centre" | "center" => Field::Centre,
            "rect" => Field::Rect,
            "presence" => Field::Presence,
            "age" => Field::Age,
            "progress" => Field::Progress,
            "text" => Field::Text,
            "index" => Field::Index,
            "count" => Field::Count,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Field::At => "at",
            Field::Size => "size",
            Field::Centre => "centre",
            Field::Rect => "rect",
            Field::Presence => "presence",
            Field::Age => "age",
            Field::Progress => "progress",
            Field::Text => "text",
            Field::Index => "index",
            Field::Count => "count",
        }
    }

    /// The fields each subject has (the rest are refused at load time,
    /// with this list in the message).
    pub fn of(subject: &Subject) -> &'static [Field] {
        use Field::*;
        match subject {
            Subject::Focus { .. } | Subject::Widget { .. } => &[At, Size, Centre, Rect, Presence],
            Subject::Cursor => &[At, Presence],
            Subject::Click { .. } => &[At, Presence, Age, Progress],
            Subject::Typing { .. } => &[At, Size, Centre, Rect, Presence, Age, Progress, Text],
            Subject::Key { .. } => &[Presence, Age, Progress, Text],
            Subject::Step => &[Presence, Age, Text, Index, Count],
            Subject::Mark { .. } => &[Presence, Age, Progress],
        }
    }
}

/// A sampled bound value.
#[derive(Clone, Debug, PartialEq)]
pub enum BindValue {
    Num(f64),
    Vec2(f64, f64),
    Vec4([f64; 4]),
    Str(String),
}

/// One bound property: a subject and the field of it.
#[derive(Clone, Debug, PartialEq)]
pub struct Bound {
    pub subject: Subject,
    pub field: Field,
}

/// How long a highlight takes to fade in or out where the subject has no
/// eased motion of its own (widgets appearing, the pointer).
pub const FADE: f64 = 0.2;

impl Bound {
    /// The value at one moment. A subject that is not there (a widget not
    /// listed, no press yet) keeps its geometry where it was last seen, or
    /// the frame's centre, with presence 0; so a layer bound to it simply
    /// shows nothing when its opacity is the presence.
    pub fn sample(&self, run: &RunData, at: &BindFrame) -> BindValue {
        let t = at.run_time;
        let rect_value = |r: Option<Rect>, presence: f64, pad: f64, age: f64, progress: f64, text: &str| -> BindValue {
            let r = r.map(|r| at.rect(&r).grow(pad)).unwrap_or_else(|| {
                let (x, y) = at.point(at.crop.centre());
                Rect::new(x, y, 0.0, 0.0)
            });
            match self.field {
                Field::At => BindValue::Vec2(r.x, r.y),
                Field::Size => BindValue::Vec2(r.w, r.h),
                Field::Centre => {
                    let (x, y) = r.centre();
                    BindValue::Vec2(x, y)
                }
                Field::Rect => BindValue::Vec4([r.x, r.y, r.w, r.h]),
                Field::Presence => BindValue::Num(presence),
                Field::Age => BindValue::Num(age),
                Field::Progress => BindValue::Num(progress),
                Field::Text => BindValue::Str(text.to_string()),
                Field::Index | Field::Count => BindValue::Num(0.0),
            }
        };
        match &self.subject {
            Subject::Focus { pad, settle } => {
                let (r, p) = run.focus(t, *settle).map_or((None, 0.0), |(r, p)| (Some(r), p));
                let p = p * r.map_or(0.0, |r| at.shown(&r).min(1.0).max(0.0).sqrt());
                rect_value(r, p, *pad, 0.0, 0.0, "")
            }
            Subject::Widget { path, pad } => {
                // Presence eases in and out over FADE around the listing.
                let now = run.widget(path, t);
                let seen = now.or_else(|| run.widget(path, t - FADE)).or_else(|| run.widget(path, t + FADE));
                let presence = match (run.widget(path, t - FADE), now, run.widget(path, t + FADE)) {
                    (_, None, _) => 0.0,
                    (None, Some(_), _) => widget_fade(run, path, t, -1.0),
                    (Some(_), Some(_), None) => widget_fade(run, path, t, 1.0),
                    _ => 1.0,
                };
                let presence = presence * seen.map_or(0.0, |r| at.shown(&r).min(1.0).max(0.0).sqrt());
                rect_value(seen, presence, *pad, 0.0, 0.0, "")
            }
            Subject::Cursor => {
                let p = run.cursor(t);
                let r = p.map(|(x, y)| Rect::new(x, y, 0.0, 0.0));
                let presence = r.map_or(0.0, |r| at.shown(&r));
                rect_value(r, presence, 0.0, 0.0, 0.0, "")
            }
            Subject::Click { nth, dur } => match run.press(t, *nth) {
                Some((press, age)) => {
                    let progress = (age / dur.max(1e-6)).clamp(0.0, 1.0);
                    let r = Rect::new(press.at.0, press.at.1, 0.0, 0.0);
                    let presence = if progress < 1.0 { at.shown(&r) } else { 0.0 };
                    rect_value(Some(r), presence, 0.0, age, progress, "")
                }
                None => rect_value(None, 0.0, 0.0, f64::INFINITY, 0.0, ""),
            },
            Subject::Typing { pad } => match run.typing(t) {
                Some(typing) => {
                    let presence = ease_window(typing.age, typing.left, FADE);
                    rect_value(typing.rect, presence, *pad, typing.age, typing.progress, &typing.text)
                }
                None => rect_value(None, 0.0, *pad, f64::INFINITY, 0.0, ""),
            },
            Subject::Key { hold } => match run.shortcut(t, *hold) {
                Some((key, age)) => {
                    let presence = ease_window(age, hold - age, FADE.min(hold * 0.25));
                    rect_value(None, presence, 0.0, age, (age / hold).clamp(0.0, 1.0), &key.label())
                }
                None => rect_value(None, 0.0, 0.0, f64::INFINITY, 0.0, ""),
            },
            Subject::Step => match run.step(t) {
                Some(step) => match self.field {
                    Field::Index => BindValue::Num(step.index as f64),
                    Field::Count => BindValue::Num(step.count as f64),
                    Field::Text => BindValue::Str(format!("{} / {}", step.index, step.count)),
                    Field::Age => BindValue::Num(step.age),
                    _ => BindValue::Num(1.0),
                },
                None => match self.field {
                    Field::Count => BindValue::Num(run.marks.len() as f64),
                    Field::Text => BindValue::Str(String::new()),
                    Field::Age => BindValue::Num(f64::INFINITY),
                    _ => BindValue::Num(0.0),
                },
            },
            Subject::Mark { name, hold } => {
                let age = run.mark(name).map_or(f64::NEG_INFINITY, |m| t - m);
                match self.field {
                    Field::Age => BindValue::Num(age),
                    Field::Progress => BindValue::Num((age / hold.max(1e-6)).clamp(0.0, 1.0)),
                    _ => BindValue::Num(if age >= 0.0 { ease_window(age, hold - age, FADE.min(hold * 0.25)) } else { 0.0 }),
                }
            }
        }
    }
}

/// 0→1 over `fade` after the start, 1→0 over `fade` before the end.
fn ease_window(age: f64, left: f64, fade: f64) -> f64 {
    if fade <= 0.0 {
        return if age >= 0.0 && left > 0.0 { 1.0 } else { 0.0 };
    }
    let a = (age / fade).clamp(0.0, 1.0);
    let b = (left / fade).clamp(0.0, 1.0);
    smooth(a.min(b))
}

fn smooth(u: f64) -> f64 {
    u * u * (3.0 - 2.0 * u)
}

/// A widget's presence near where it appears (`dir` -1: it appeared within
/// FADE before `t`) or goes (`dir` 1: it goes within FADE after `t`): the
/// seconds from `t` to that frame's edge, eased over FADE.
fn widget_fade(run: &RunData, path: &str, t: f64, dir: f64) -> f64 {
    let step = 1.0 / run.fps;
    let frame = run.frame_at(t) as f64;
    let mut n = 1.0;
    while n * step <= FADE + step {
        let probe = frame + dir * n;
        if probe < 0.0 || run.widget(path, (probe + 0.5) * step).is_none() {
            // The first frame it is not listed on (going), or the last one
            // before it was (appearing): its edge towards `t`.
            let edge = if dir > 0.0 { probe * step } else { (probe + 1.0) * step };
            return smooth(((edge - t).abs() / FADE).clamp(0.0, 1.0));
        }
        n += 1.0;
    }
    1.0
}

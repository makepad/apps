//! The script calls as data. [`Call::parse`] does all
//! argument checking (names, options, ranges, defaults) once; AppRun's
//! script natives and DECK's step records both end in it, and a
//! [`Runner`](crate::runner::Runner) executes the result.

use crate::driver::Mods;
use crate::values::{read, Sv};
use makepad_script::*;

/// Defaults of the call options (seconds unless noted).
pub const MOVE_TIME: f64 = 0.35;
pub const DRAG_TIME: f64 = 0.5;
pub const SCROLL_TIME: f64 = 0.3;
pub const TYPE_CPS: f64 = 12.0;
pub const WAIT_MAX: f64 = 5.0;
pub const FOCUS_HOLD: f64 = 1.0;

/// Where a call acts: a widget path, or a point in window points.
#[derive(Clone, Debug, PartialEq)]
pub enum Target {
    Path(String),
    Point(f64, f64),
}

impl Target {
    pub fn from_sv(v: &Sv, what: &str) -> Result<Target, String> {
        match v {
            Sv::Id(p) | Sv::Str(p) if !p.is_empty() => Ok(Target::Path(p.clone())),
            Sv::Arr(xy) if xy.len() == 2 => match (xy[0].num(), xy[1].num()) {
                (Some(x), Some(y)) => Ok(Target::Point(x, y)),
                _ => Err(format!("{what} is a widget path (@a.b) or vec2(x, y) with numbers")),
            },
            other => Err(format!("{what} is a widget path (@a.b) or vec2(x, y), not {}", other.kind())),
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Target::Path(p) => format!("@{p}"),
            Target::Point(x, y) => format!("vec2({x}, {y})"),
        }
    }
}

/// What `wait_state` waits for in a selection.
#[derive(Clone, Debug, PartialEq)]
pub enum SelWant {
    /// This item is selected.
    Item(String),
    /// Anything is selected.
    Some,
    /// Nothing is selected.
    Nothing,
}

/// The states `wait_state` waits for; at least one is set, all must hold.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StateWant {
    pub enabled: Option<bool>,
    pub checked: Option<bool>,
    pub selected: Option<SelWant>,
    /// The widget's value is exactly this.
    pub value: Option<String>,
}

/// The typing rate of a call played as fast as it goes (characters per second).
const FAST_CPS: f64 = 1.0e9;

#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    Click { target: Target, button: u8, count: u64, move_time: f64 },
    Drag { from: Target, to: Target, time: f64, move_time: f64 },
    Scroll { target: Target, dx: f64, dy: f64, time: f64, move_time: f64 },
    Type { target: Target, text: String, cps: f64, move_time: f64 },
    Key { code: String, mods: Mods },
    Drop { target: Target, path: String, move_time: f64 },
    Resize { width: f64, height: f64 },
    Hold { seconds: f64 },
    WaitSettled { max: f64 },
    WaitText { path: String, text: String, max: f64 },
    WaitVisible { path: String, max: f64 },
    WaitState { path: String, want: StateWant, max: f64 },
    Mark { name: String },
    Cut,
    Resume,
    Focus { target: Target, pad: f64, weight: f64, hold: f64 },
    FocusRect { rect: [f64; 4], pad: f64, weight: f64, hold: f64 },
    Wide { hold: f64 },
}

/// A call's positional arguments, and whether an options object follows.
fn shape(name: &str) -> Option<(&'static [&'static str], bool)> {
    Some(match name {
        "click" => (&["target"], true),
        "drag" => (&["from", "to"], true),
        "scroll" => (&["target", "dx", "dy"], true),
        "type" => (&["target", "text"], true),
        "key" => (&["code"], true),
        "drop" => (&["target", "path"], true),
        "resize" => (&["width", "height"], false),
        "hold" => (&["seconds"], false),
        "wait_settled" => (&[], true),
        "wait_text" => (&["target", "text"], true),
        "wait_visible" => (&["target"], true),
        "wait_state" => (&["target"], true),
        "mark" => (&["name"], false),
        "cut" | "resume" => (&[], false),
        "focus" => (&["target"], true),
        "focus_rect" => (&["rect"], true),
        "wide" => (&[], true),
        _ => return None,
    })
}

/// Call options: an object with only the listed keys, or nil.
struct Opts<'a> {
    v: &'a Sv,
}

impl<'a> Opts<'a> {
    fn new(v: &'a Sv, allowed: &[&str], call: &str) -> Result<Opts<'a>, String> {
        match v {
            Sv::Nil => Ok(Opts { v }),
            Sv::Obj(fields) => {
                for (key, _) in fields {
                    if !allowed.contains(&key.as_str()) {
                        return Err(format!("{call}: unknown option `{key}` (options: {})", allowed.join(", ")));
                    }
                }
                Ok(Opts { v })
            }
            other => Err(format!("{call}: options are an object {{...}}, not {}", other.kind())),
        }
    }

    fn num(&self, key: &str, default: f64, min: f64, max: f64) -> Result<f64, String> {
        match self.v.field(key) {
            None | Some(Sv::Nil) => Ok(default),
            Some(v) => match v.num() {
                Some(n) if (min..=max).contains(&n) => Ok(n),
                _ => Err(format!("option `{key}` is a number from {min} to {max}")),
            },
        }
    }

    fn flag(&self, key: &str) -> Result<bool, String> {
        match self.v.field(key) {
            None | Some(Sv::Nil) => Ok(false),
            Some(Sv::Bool(b)) => Ok(*b),
            Some(other) => Err(format!("option `{key}` is true or false, not {}", other.kind())),
        }
    }

    fn opt_flag(&self, key: &str) -> Result<Option<bool>, String> {
        match self.v.field(key) {
            None | Some(Sv::Nil) => Ok(None),
            Some(Sv::Bool(b)) => Ok(Some(*b)),
            Some(other) => Err(format!("option `{key}` is true or false, not {}", other.kind())),
        }
    }
}

fn number(v: &Sv, what: &str, min: f64, max: f64) -> Result<f64, String> {
    match v.num() {
        Some(n) if (min..=max).contains(&n) => Ok(n),
        _ => Err(format!("{what} is a number from {min} to {max}, not {v:?}")),
    }
}

fn name_of(v: &Sv, what: &str) -> Result<String, String> {
    match v {
        Sv::Id(s) | Sv::Str(s) if !s.is_empty() => Ok(s.clone()),
        other => Err(format!("{what} is an @id or a string, not {}", other.kind())),
    }
}

fn string_of(v: &Sv, call: &str, what: &str) -> Result<String, String> {
    match v {
        Sv::Str(s) => Ok(s.clone()),
        other => Err(format!("{call}: {what} is a string, not {}", other.kind())),
    }
}

fn path_of(v: &Sv, call: &str) -> Result<String, String> {
    match Target::from_sv(v, &format!("{call} target"))? {
        Target::Path(p) => Ok(p),
        Target::Point(..) => Err(format!("{call} waits on a widget path, not a point")),
    }
}

impl Call {
    /// The call paced for playback at `speed` times its written pace
    /// (`f64::INFINITY`: as fast as it goes). Holds, move times, drag and
    /// scroll times and the typing rate scale; waits and their timeouts do
    /// not, since they are the app's to meet, not the audience's to watch.
    /// Runner-fixed times (a press, a key's down and up, the gap that keeps
    /// separate clicks from being a double click) are the app's too.
    pub fn at_speed(&self, speed: f64) -> Call {
        let speed = if speed > 0.0 { speed } else { 1.0 };
        let time = |seconds: f64| if speed.is_infinite() { 0.0 } else { seconds / speed };
        let rate = |cps: f64| if speed.is_infinite() { FAST_CPS } else { cps * speed };
        let mut call = self.clone();
        match &mut call {
            Call::Click { move_time, .. } | Call::Drop { move_time, .. } => *move_time = time(*move_time),
            Call::Drag { time: t, move_time, .. } | Call::Scroll { time: t, move_time, .. } => {
                *t = time(*t);
                *move_time = time(*move_time);
            }
            Call::Type { cps, move_time, .. } => {
                *cps = rate(*cps);
                *move_time = time(*move_time);
            }
            Call::Hold { seconds } => *seconds = time(*seconds),
            Call::Focus { hold, .. } | Call::FocusRect { hold, .. } | Call::Wide { hold } => *hold = time(*hold),
            _ => {}
        }
        call
    }

    /// The call's script name.
    pub fn name(&self) -> &'static str {
        match self {
            Call::Click { .. } => "click",
            Call::Drag { .. } => "drag",
            Call::Scroll { .. } => "scroll",
            Call::Type { .. } => "type",
            Call::Key { .. } => "key",
            Call::Drop { .. } => "drop",
            Call::Resize { .. } => "resize",
            Call::Hold { .. } => "hold",
            Call::WaitSettled { .. } => "wait_settled",
            Call::WaitText { .. } => "wait_text",
            Call::WaitVisible { .. } => "wait_visible",
            Call::WaitState { .. } => "wait_state",
            Call::Mark { .. } => "mark",
            Call::Cut => "cut",
            Call::Resume => "resume",
            Call::Focus { .. } => "focus",
            Call::FocusRect { .. } => "focus_rect",
            Call::Wide { .. } => "wide",
        }
    }

    /// Every target the call names, in order (validate mode resolves them
    /// against the launch dump).
    pub fn targets(&self) -> Vec<Target> {
        match self {
            Call::Click { target, .. } | Call::Scroll { target, .. } | Call::Type { target, .. } | Call::Drop { target, .. } | Call::Focus { target, .. } => {
                vec![target.clone()]
            }
            Call::Drag { from, to, .. } => vec![from.clone(), to.clone()],
            Call::WaitText { path, .. } | Call::WaitVisible { path, .. } | Call::WaitState { path, .. } => vec![Target::Path(path.clone())],
            _ => Vec::new(),
        }
    }

    /// The target a call's record names: the first one.
    pub fn record_target(&self) -> Option<Target> {
        self.targets().into_iter().next()
    }

    /// `args` are the call's arguments in script order: the positional ones,
    /// then the options object (missing ones are nil; extra ones are an
    /// error). `Sv::Nil` options mean defaults.
    pub fn parse(name: &str, args: &[Sv]) -> Result<Call, String> {
        let (positional, options) = shape(name).ok_or_else(|| format!("unknown call `{name}`"))?;
        let arity = positional.len() + options as usize;
        if args.len() > arity {
            return Err(format!("{name}: takes {} arguments ({}{}), got {}", arity, positional.join(", "), if options { ", {options}" } else { "" }, args.len()));
        }
        let mut a: Vec<Sv> = args.to_vec();
        a.resize(arity, Sv::Nil);
        let move_time = |o: &Opts| o.num("move_time", MOVE_TIME, 0.0, 10.0);
        Ok(match name {
            "click" => {
                let target = Target::from_sv(&a[0], "click target")?;
                let o = Opts::new(&a[1], &["button", "count", "move_time"], "click")?;
                let button = match a[1].field("button") {
                    None | Some(Sv::Nil) => 0,
                    Some(Sv::Id(b)) | Some(Sv::Str(b)) => match b.as_str() {
                        "left" => 0,
                        "right" => 1,
                        "middle" => 2,
                        _ => return Err("click: button is @left, @right or @middle".into()),
                    },
                    Some(v) => number(v, "click: button", 0.0, 2.0)? as u8,
                };
                Call::Click { target, button, count: o.num("count", 1.0, 1.0, 3.0)? as u64, move_time: move_time(&o)? }
            }
            "drag" => {
                let from = Target::from_sv(&a[0], "drag from")?;
                let to = Target::from_sv(&a[1], "drag to")?;
                let o = Opts::new(&a[2], &["time", "move_time"], "drag")?;
                Call::Drag { from, to, time: o.num("time", DRAG_TIME, 0.0, 60.0)?, move_time: move_time(&o)? }
            }
            "scroll" => {
                let target = Target::from_sv(&a[0], "scroll target")?;
                let dx = number(&a[1], "scroll dx", -1e6, 1e6)?;
                let dy = number(&a[2], "scroll dy", -1e6, 1e6)?;
                let o = Opts::new(&a[3], &["time", "move_time"], "scroll")?;
                Call::Scroll { target, dx, dy, time: o.num("time", SCROLL_TIME, 0.0, 60.0)?, move_time: move_time(&o)? }
            }
            "type" => {
                let target = Target::from_sv(&a[0], "type target")?;
                let text = string_of(&a[1], "type", "text")?;
                let o = Opts::new(&a[2], &["cps", "move_time"], "type")?;
                Call::Type { target, text, cps: o.num("cps", TYPE_CPS, 0.1, 1000.0)?, move_time: move_time(&o)? }
            }
            "key" => {
                let code = name_of(&a[0], "key code")?;
                let o = Opts::new(&a[1], &["cmd", "shift", "alt", "ctrl"], "key")?;
                Call::Key { code, mods: Mods { cmd: o.flag("cmd")?, shift: o.flag("shift")?, alt: o.flag("alt")?, ctrl: o.flag("ctrl")? } }
            }
            "drop" => {
                let target = Target::from_sv(&a[0], "drop target")?;
                let path = string_of(&a[1], "drop", "path")?;
                let o = Opts::new(&a[2], &["move_time"], "drop")?;
                Call::Drop { target, path, move_time: move_time(&o)? }
            }
            "resize" => Call::Resize { width: number(&a[0], "resize width", 1.0, 16384.0)?, height: number(&a[1], "resize height", 1.0, 16384.0)? },
            "hold" => Call::Hold { seconds: number(&a[0], "hold seconds", 0.0, 3600.0)? },
            "wait_settled" => Call::WaitSettled { max: Opts::new(&a[0], &["max"], "wait_settled")?.num("max", WAIT_MAX, 0.0, 600.0)? },
            "wait_text" => {
                let path = path_of(&a[0], "wait_text")?;
                let text = string_of(&a[1], "wait_text", "text")?;
                Call::WaitText { path, text, max: Opts::new(&a[2], &["max"], "wait_text")?.num("max", WAIT_MAX, 0.0, 600.0)? }
            }
            "wait_visible" => {
                let path = path_of(&a[0], "wait_visible")?;
                Call::WaitVisible { path, max: Opts::new(&a[1], &["max"], "wait_visible")?.num("max", WAIT_MAX, 0.0, 600.0)? }
            }
            "wait_state" => {
                let path = path_of(&a[0], "wait_state")?;
                let o = Opts::new(&a[1], &["enabled", "checked", "selected", "value", "max"], "wait_state")?;
                let selected = match a[1].field("selected") {
                    None | Some(Sv::Nil) => None,
                    Some(Sv::Bool(true)) => Some(SelWant::Some),
                    Some(Sv::Bool(false)) => Some(SelWant::Nothing),
                    Some(Sv::Str(s)) | Some(Sv::Id(s)) => Some(SelWant::Item(s.clone())),
                    Some(other) => return Err(format!("wait_state: selected is true, false or an item name, not {}", other.kind())),
                };
                let value = match a[1].field("value") {
                    None | Some(Sv::Nil) => None,
                    Some(Sv::Str(s)) => Some(s.clone()),
                    Some(Sv::Num(n)) => Some(format!("{n}")),
                    Some(other) => return Err(format!("wait_state: value is a string or a number, not {}", other.kind())),
                };
                let want = StateWant { enabled: o.opt_flag("enabled")?, checked: o.opt_flag("checked")?, selected, value };
                if want == StateWant::default() {
                    return Err("wait_state: name a state to wait for (options: enabled, checked, selected, value)".into());
                }
                Call::WaitState { path, want, max: o.num("max", WAIT_MAX, 0.0, 600.0)? }
            }
            "mark" => Call::Mark { name: name_of(&a[0], "mark name")? },
            "cut" => Call::Cut,
            "resume" => Call::Resume,
            "focus" => {
                let target = Target::from_sv(&a[0], "focus target")?;
                let o = Opts::new(&a[1], &["pad", "weight", "hold"], "focus")?;
                Call::Focus { target, pad: o.num("pad", 0.0, 0.0, 4096.0)?, weight: o.num("weight", 1.0, 0.0, 1.0)?, hold: o.num("hold", FOCUS_HOLD, 0.0, 3600.0)? }
            }
            "focus_rect" => {
                let rect = match &a[0] {
                    Sv::Arr(v) if v.len() == 4 && v.iter().all(|n| n.num().is_some()) => {
                        let n: Vec<f64> = v.iter().map(|n| n.num().unwrap()).collect();
                        if n[2] < 0.0 || n[3] < 0.0 {
                            return Err("focus_rect: width and height are >= 0".into());
                        }
                        [n[0], n[1], n[2], n[3]]
                    }
                    other => return Err(format!("focus_rect takes rect(x, y, w, h) in window points, not {}", other.kind())),
                };
                let o = Opts::new(&a[1], &["pad", "weight", "hold"], "focus_rect")?;
                Call::FocusRect { rect, pad: o.num("pad", 0.0, 0.0, 4096.0)?, weight: o.num("weight", 1.0, 0.0, 1.0)?, hold: o.num("hold", FOCUS_HOLD, 0.0, 3600.0)? }
            }
            "wide" => Call::Wide { hold: Opts::new(&a[0], &["hold"], "wide")?.num("hold", FOCUS_HOLD, 0.0, 3600.0)? },
            _ => unreachable!("shape() lists exactly these calls"),
        })
    }
}

/// A step record, as DECK's `mod.deck` constructors produce it:
///
/// ```text
/// {call: @click  target: @counter.increment  count: 3}
/// {call: @type   target: @main.new_note  text: "hello"  cps: 20}
/// {call: @hold   seconds: 0.5}
/// ```
///
/// `call` names the script call (`click drag scroll type key drop resize
/// hold wait_settled wait_text wait_visible wait_state mark cut resume
/// focus focus_rect wide`). The call's positional arguments are fields of
/// the same names as the script's (`target`, `from`/`to`, `dx`/`dy`, `text`,
/// `code`, `path`, `width`/`height`, `seconds`, `name`, `rect`); every other
/// field is an option (`button count move_time time cps cmd shift alt ctrl
/// max pad weight hold enabled checked selected value`). The record goes
/// through [`Call::parse`], so names, ranges, defaults and errors are the
/// script's.
pub fn call_from_record(vm: &ScriptVm, value: ScriptValue) -> Result<Call, String> {
    call_from_sv(&read(vm, value))
}

pub fn call_from_sv(record: &Sv) -> Result<Call, String> {
    let Sv::Obj(fields) = record else {
        return Err(format!("a step is a record {{call: @name ...}}, not {}", record.kind()));
    };
    let name = match record.field("call") {
        Some(Sv::Id(n)) | Some(Sv::Str(n)) => n.clone(),
        _ => return Err("a step record has `call: @name`".into()),
    };
    let (positional, options) = shape(&name).ok_or_else(|| format!("unknown call `{name}`"))?;
    let mut args: Vec<Sv> = positional.iter().map(|p| record.field(p).cloned().unwrap_or(Sv::Nil)).collect();
    let rest: Vec<(String, Sv)> = fields.iter().filter(|(k, _)| k != "call" && !positional.contains(&k.as_str())).cloned().collect();
    if options {
        args.push(if rest.is_empty() { Sv::Nil } else { Sv::Obj(rest) });
    } else if let Some((key, _)) = rest.first() {
        return Err(format!("{name}: unknown field `{key}` (fields: {})", positional.join(", ")));
    }
    Call::parse(&name, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Sv {
        Sv::Id(s.into())
    }
    fn obj(fields: &[(&str, Sv)]) -> Sv {
        Sv::Obj(fields.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
    }

    #[test]
    fn defaults_and_options_are_checked_once() {
        let c = Call::parse("click", &[p("a.b")]).unwrap();
        assert_eq!(c, Call::Click { target: Target::Path("a.b".into()), button: 0, count: 1, move_time: MOVE_TIME });
        let c = Call::parse("click", &[p("a.b"), obj(&[("count", Sv::Num(3.0)), ("button", p("right"))])]).unwrap();
        assert!(matches!(c, Call::Click { button: 1, count: 3, .. }));
        assert!(Call::parse("click", &[p("a"), obj(&[("cout", Sv::Num(2.0))])]).unwrap_err().contains("unknown option `cout`"));
        assert!(Call::parse("click", &[p("a"), obj(&[("count", Sv::Num(9.0))])]).unwrap_err().contains("from 1 to 3"));
        assert!(Call::parse("type", &[p("a"), Sv::Num(5.0)]).unwrap_err().contains("text is a string"));
        assert!(Call::parse("wait_visible", &[Sv::Arr(vec![Sv::Num(1.0), Sv::Num(2.0)])]).unwrap_err().contains("not a point"));
        assert!(Call::parse("clack", &[]).is_err());
        assert!(Call::parse("hold", &[Sv::Num(1.0), Sv::Num(2.0)]).unwrap_err().contains("takes 1 arguments"));
    }

    #[test]
    fn wait_state_needs_a_state() {
        assert!(Call::parse("wait_state", &[p("a.b")]).unwrap_err().contains("name a state"));
        let c = Call::parse("wait_state", &[p("a.b"), obj(&[("enabled", Sv::Bool(true)), ("value", Sv::Num(4.0)), ("max", Sv::Num(2.0))])]).unwrap();
        assert_eq!(c, Call::WaitState { path: "a.b".into(), want: StateWant { enabled: Some(true), checked: None, selected: None, value: Some("4".into()) }, max: 2.0 });
        assert!(Call::parse("wait_state", &[p("a.b"), obj(&[("enable", Sv::Bool(true))])]).unwrap_err().contains("unknown option"));
    }

    #[test]
    fn a_record_parses_like_the_script_call() {
        let record = obj(&[("call", p("click")), ("target", p("counter.increment")), ("count", Sv::Num(3.0))]);
        assert_eq!(call_from_sv(&record).unwrap(), Call::parse("click", &[p("counter.increment"), obj(&[("count", Sv::Num(3.0))])]).unwrap());
        let record = obj(&[("call", p("hold")), ("seconds", Sv::Num(0.5))]);
        assert_eq!(call_from_sv(&record).unwrap(), Call::Hold { seconds: 0.5 });
        assert!(call_from_sv(&obj(&[("call", p("hold")), ("seconds", Sv::Num(0.5)), ("max", Sv::Num(1.0))])).unwrap_err().contains("unknown field `max`"));
        assert!(call_from_sv(&obj(&[("target", p("a"))])).unwrap_err().contains("call: @name"));
        assert!(call_from_sv(&Sv::Nil).is_err());
    }

    #[test]
    fn speed_scales_the_pace_and_never_the_waits() {
        let click = Call::Click { target: Target::Path("a".into()), button: 0, count: 1, move_time: 0.4 };
        assert_eq!(click.at_speed(2.0), Call::Click { target: Target::Path("a".into()), button: 0, count: 1, move_time: 0.2 });
        assert_eq!(Call::Hold { seconds: 1.0 }.at_speed(0.5), Call::Hold { seconds: 2.0 });
        assert_eq!(Call::Hold { seconds: 1.0 }.at_speed(f64::INFINITY), Call::Hold { seconds: 0.0 });
        let typed = Call::Type { target: Target::Path("a".into()), text: "hi".into(), cps: 10.0, move_time: 0.3 };
        let Call::Type { cps, move_time, .. } = typed.at_speed(1.5) else { unreachable!() };
        assert!((cps - 15.0).abs() < 1e-9 && (move_time - 0.2).abs() < 1e-9);
        let Call::Type { cps, move_time, .. } = typed.at_speed(f64::INFINITY) else { unreachable!() };
        assert!(cps >= 1.0e6 && move_time == 0.0);
        let wait = Call::WaitText { path: "a".into(), text: "4".into(), max: 7.0 };
        assert_eq!(wait.at_speed(2.0), wait);
        assert_eq!(wait.at_speed(f64::INFINITY), wait);
        let drag = Call::Drag { from: Target::Point(0.0, 0.0), to: Target::Point(1.0, 1.0), time: 0.8, move_time: 0.4 };
        assert_eq!(drag.at_speed(f64::INFINITY), Call::Drag { from: Target::Point(0.0, 0.0), to: Target::Point(1.0, 1.0), time: 0.0, move_time: 0.0 });
    }
}

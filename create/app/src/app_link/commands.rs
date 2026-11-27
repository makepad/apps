use super::{AppWindow, WindowId};
use makepad_widgets::KeyCode;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub logo: bool,
}

#[derive(Clone, Debug)]
pub enum AppInput {
    PointerMove {
        x: f64,
        y: f64,
    },
    PointerDown {
        x: f64,
        y: f64,
        button: u8,
        mods: Mods,
    },
    PointerUp {
        x: f64,
        y: f64,
        button: u8,
        mods: Mods,
    },
    Scroll {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
        mods: Mods,
    },
    KeyDown {
        key: KeyCode,
        mods: Mods,
    },
    KeyUp {
        key: KeyCode,
        mods: Mods,
    },
    Text(String),
}

#[derive(Clone, Copy, Debug)]
pub enum DesignVerb {
    Open,
    State,
    Palette,
    Insert,
    Move,
    Delete,
    Duplicate,
    Wrap,
    Up,
    Down,
    Out,
    Rename,
    Set,
    Bake,
    Undo,
    Redo,
    Reset,
    Patch,
    Commit,
    Verify,
    Close,
    Pick,
    Tree,
    Props,
    Apply,
    Typed,
    Locate,
    Theme,
    States,
    Pose,
}

#[derive(Clone, Debug)]
pub enum DesignOp {
    Overlay(bool),
    Annotate(bool),
    State,
    Diff,
    Clear,
    Final,
    ApplyProp {
        path: String,
        prop: String,
        value: String,
    },
    ApplySplash {
        path: String,
        splash: String,
    },
    Design {
        verb: DesignVerb,
        args: Vec<(String, String)>,
    },
}

type Args = Vec<(String, String)>;

pub(super) fn line(cmd: &str, args: &[(String, String)]) -> String {
    makepad_remote::app::request_line(&[cmd.to_string()], args)
}

fn field(k: &str, v: impl ToString) -> (String, String) {
    (k.into(), v.to_string())
}

impl AppInput {
    pub(super) fn lines(&self, window: &AppWindow, seq: u64) -> Result<Vec<String>, &'static str> {
        if let Self::PointerMove { x, y } | Self::PointerDown { x, y, .. }
            | Self::PointerUp { x, y, .. } | Self::Scroll { x, y, .. } = self
        {
            if !x.is_finite() || !y.is_finite() {
                return Err("non-finite pointer coordinate");
            }
            let (w, h) = window.size;
            let outside = if matches!(self, Self::PointerMove { .. } | Self::PointerUp { .. }) {
                *x < -w || *x > 2. * w || *y < -h || *y > 2. * h
            } else {
                *x < 0. || *x > w || *y < 0. || *y > h
            };
            if outside {
                return Err("pointer outside permitted bounds");
            }
        }
        self.command_lines(window.id, seq)
    }

    // The worker also uses this to undo a previously applied press at its actual
    // last position. The window can have resized since those coordinates were checked.
    pub(super) fn command_lines(&self, window: WindowId, seq: u64) -> Result<Vec<String>, &'static str> {
        let mut args = vec![field("w", window.0), field("if_user_seq", seq)];
        let mut mods = Mods::default();
        let cmd = match self {
            Self::Text(t) => {
                // Bound work per input event. Split only on UTF-8 boundaries and send every piece.
                let mut text = t.as_str();
                let mut chunks = Vec::new();
                loop {
                    let mut end = text.len().min(4096);
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    args.push(field("t", &text[..end]));
                    chunks.push(line("text", &args));
                    args.pop();
                    text = &text[end..];
                    if text.is_empty() {
                        return Ok(chunks);
                    }
                }
            }
            Self::KeyDown { key, mods: m } | Self::KeyUp { key, mods: m } => {
                mods = *m;
                args.push(field(
                    "k",
                    if matches!(self, Self::KeyDown { .. }) {
                        "down"
                    } else {
                        "up"
                    },
                ));
                let code = format!("{key:?}");
                if *key == KeyCode::Unknown || KeyCode::from_name(&code) != Some(*key) {
                    return Err("unknown key code");
                }
                args.push(field("c", code));
                "key"
            }
            Self::PointerMove { x, y }
            | Self::PointerDown { x, y, .. }
            | Self::PointerUp { x, y, .. }
            | Self::Scroll { x, y, .. } => {
                args.push(field("x", x));
                args.push(field("y", y));
                let kind = match self {
                    Self::PointerDown {
                        button, mods: m, ..
                    }
                    | Self::PointerUp {
                        button, mods: m, ..
                    } => {
                        if *button > 2 {
                            return Err("invalid pointer button");
                        }
                        args.push(field("b", button));
                        mods = *m;
                        if matches!(self, Self::PointerDown { .. }) {
                            "down"
                        } else {
                            "up"
                        }
                    }
                    Self::Scroll {
                        dx, dy, mods: m, ..
                    } => {
                        if !dx.is_finite() || !dy.is_finite() {
                            return Err("non-finite scroll distance");
                        }
                        args.extend([field("dx", dx), field("dy", dy)]);
                        mods = *m;
                        "scroll"
                    }
                    _ => "move",
                };
                args.push(field("k", kind));
                "mouse"
            }
        };
        for (key, on) in [
            ("shift", mods.shift),
            ("ctrl", mods.control),
            ("alt", mods.alt),
            ("logo", mods.logo),
        ] {
            if on {
                args.push(field(key, 1));
            }
        }
        Ok(vec![line(cmd, &args)])
    }
}

impl DesignOp {
    pub(super) fn check_window(&self, window: Option<WindowId>) -> Result<(), String> {
        if window.is_none() { return Ok(()) }
        let name = match self {
            Self::Design { verb: DesignVerb::Pick | DesignVerb::Tree | DesignVerb::Props | DesignVerb::Apply | DesignVerb::Typed |
                DesignVerb::Locate | DesignVerb::Theme | DesignVerb::States | DesignVerb::Pose, .. } => return Ok(()),
            Self::Design { verb, .. } => return Err(format!("design {} takes no window", format!("{verb:?}").to_ascii_lowercase())),
            Self::Overlay(_) | Self::Annotate(_) => "tweak",
            Self::State => "tweak state",
            Self::Diff => "tweak diff",
            Self::Clear => "tweak clear",
            Self::Final => "tweak final",
            Self::ApplyProp { .. } | Self::ApplySplash { .. } => "tweak apply",
        };
        Err(format!("{name} takes no window"))
    }
}

impl DesignOp {
    pub(super) fn line(self, window: Option<WindowId>) -> Result<String, String> {
        self.check_window(window)?;
        let (cmd, mut args): (String, Args) = match self {
            Self::Overlay(on) => ("tweak".into(), vec![field("on", on as u8)]),
            Self::Annotate(on) => ("tweak".into(), vec![field("annotate", on as u8)]),
            Self::State => ("tweak state".into(), vec![]),
            Self::Diff => ("tweak diff".into(), vec![]),
            Self::Clear => ("tweak clear".into(), vec![]),
            Self::Final => ("tweak final".into(), vec![]),
            Self::ApplyProp { path, prop, value } => (
                "tweak apply".into(),
                vec![
                    field("path", path),
                    field("prop", prop),
                    field("value", value),
                ],
            ),
            Self::ApplySplash { path, splash } => (
                "tweak apply".into(),
                vec![field("path", path), field("splash", splash)],
            ),
            Self::Design { verb, args } => {
                let op = format!("{verb:?}").to_ascii_lowercase();
                let mut seen = std::collections::HashSet::new();
                for (key, _) in &args {
                    if key.is_empty()
                        || !key
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                        || matches!(key.as_str(), "cmd" | "w" | "window" | "if_user_seq")
                        || !seen.insert(key)
                    {
                        return Err(format!("invalid or repeated design argument {key:?}"));
                    }
                }
                if let Some(schema) = makepad_remote::params::design(&op) {
                    makepad_remote::params::validate(args.iter().map(|p| p.0.as_str()), schema)?;
                }
                (format!("design {op}"), args)
            }
        };
        if let Some(w) = window {
            args.push(field("w", w.0));
        }
        Ok(line(&cmd, &args))
    }
}

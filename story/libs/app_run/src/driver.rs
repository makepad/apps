//! The only I/O of a call: what the [`Runner`](crate::runner::Runner)
//! asks of the app it drives. [`Session`](crate::session::Session) implements
//! it over the remote routes on the virtual clock; DECK implements it over
//! a hosted live app on the real clock. The runner's call semantics (cursor
//! moves, press timing, typing at cps, waits with deadlines) run unchanged
//! over either.

use std::path::Path;

/// One widget of the app, in window points. `path` is the widget id path
/// (`main.new_note`) when the app registered paths.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub path: Option<String>,
    pub id: String,
    pub ty: String,
    pub rect: [f64; 4],
    pub text: Option<String>,
    pub value: Option<String>,
    pub visible: bool,
    /// None: the driver does not report it.
    pub enabled: Option<bool>,
    pub checked: Option<bool>,
    pub selected: Option<String>,
}

impl Row {
    /// A visible, enabled row with a rect and nothing else (drivers fill in
    /// what they know).
    pub fn new(path: Option<String>, id: &str, ty: &str, rect: [f64; 4]) -> Row {
        Row {
            path,
            id: id.to_string(),
            ty: ty.to_string(),
            rect,
            text: None,
            value: None,
            visible: rect[2] > 0.0 && rect[3] > 0.0,
            enabled: None,
            checked: None,
            selected: None,
        }
    }

    /// The runner's geometry and assertions, read from the shared semantic node.
    pub fn from_node(node: &makepad_a11y::Node) -> Row {
        use makepad_a11y::{Role, States, Toggle};
        let bounds = node.bounds;
        let mut row = Row::new(
            (!node.path.is_empty()).then(|| node.path.clone()),
            &node.id.short(),
            &node.class,
            [bounds.x as f64, bounds.y as f64, bounds.w as f64, bounds.h as f64],
        );
        row.text = Some(node.text.as_ref().map_or_else(|| {
            if node.name.is_empty() { node.value.clone() } else { node.name.clone() }
        }, |text| text.content.clone()));
        row.value = Some(node.value.clone());
        row.visible &= !node.states.intersects(States::HIDDEN | States::OFFSCREEN);
        row.enabled = Some(!node.states.contains(States::DISABLED));
        row.checked = match node.toggle { Toggle::On => Some(true), Toggle::Off => Some(false), _ => None };
        row.selected = if node.states.contains(States::SELECTED) {
            Some(if node.name.is_empty() { node.id.short() } else { node.name.clone() })
        } else if matches!(node.role, Role::ComboBox | Role::TabList | Role::List | Role::Tree | Role::Table | Role::Grid) {
            (!node.value.is_empty()).then(|| node.value.clone())
        } else {
            None
        };
        row
    }

    pub fn center(&self) -> (f64, f64) {
        (self.rect[0] + self.rect[2] * 0.5, self.rect[1] + self.rect[3] * 0.5)
    }

    pub fn name(&self) -> String {
        self.path.clone().unwrap_or_else(|| format!("{} ({})", self.id, self.ty))
    }
}

/// A pointer event. Buttons: 0 left, 1 right, 2 middle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mouse {
    Move,
    Down(u8),
    Up(u8),
    Scroll(f64, f64),
}

/// Held modifier keys.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Mods {
    pub cmd: bool,
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    /// The held modifiers by name, in the order cmd, shift, alt, ctrl.
    pub fn names(&self) -> Vec<String> {
        [("cmd", self.cmd), ("shift", self.shift), ("alt", self.alt), ("ctrl", self.ctrl)]
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name.to_string())
            .collect()
    }

    /// Canonical parameter names for the inspect input commands.
    pub fn remote_params(&self) -> impl Iterator<Item = &'static str> {
        [("logo", self.cmd), ("shift", self.shift), ("alt", self.alt), ("ctrl", self.ctrl)]
            .into_iter()
            .filter(|(_, on)| *on)
            .map(|(name, _)| name)
    }

    /// The remote's query form, `&logo=1&shift=1`.
    pub fn query(&self) -> String {
        self.remote_params().map(|m| format!("&{m}=1")).collect()
    }
}

/// An app built before its widgets layer registered id paths.
pub const NO_PATHS: &str = "the app reports no widget paths: its binary predates widget path addressing (rebuild it)";

pub trait Driver {
    /// Whether the app's answers are current. A driver whose reads are
    /// answered later says `false` until its semantic tree has arrived: the runner then returns `Pending` without letting a
    /// frame count against the wait, and looks again on a later poll. The
    /// `snapshot`, `find` and `settled` that follow read that answer.
    /// Drivers that answer at once need not override it.
    fn ready(&mut self) -> bool {
        true
    }

    /// Every visible widget.
    fn snapshot(&mut self) -> Result<Vec<Row>, String>;

    /// Every visible widget whose path is `path` or ends in it by whole
    /// segments. A driver with a cheaper targeted query overrides this.
    fn find(&mut self, path: &str) -> Result<Vec<Row>, String> {
        Ok(self.snapshot()?.into_iter().filter(|r| r.path.as_deref().is_some_and(|p| crate::plan::path_matches(path, p))).collect())
    }

    /// A pointer event at a point in window points.
    fn mouse(&mut self, kind: Mouse, x: f64, y: f64) -> Result<(), String>;

    /// A key going down (`down`) or up.
    fn key(&mut self, down: bool, code: &str, mods: Mods) -> Result<(), String>;

    fn text(&mut self, text: &str) -> Result<(), String>;

    fn drop_file(&mut self, path: &Path, x: f64, y: f64) -> Result<(), String>;

    /// Window size in points.
    fn resize(&mut self, w: f64, h: f64) -> Result<(), String>;

    /// Whether nothing is pending (animation, redraw, loads) and what is.
    fn settled(&mut self) -> Result<(bool, Vec<String>), String>;

    /// Virtual clock only: present `k` frames. A real-clock driver is never
    /// asked (time passes by itself) and may refuse.
    fn advance_frames(&mut self, k: u64) -> Result<(), String>;

    /// Stop (`true`) or restart a recording; a driver that does not record
    /// refuses.
    fn capture_pause(&mut self, _pause: bool) -> Result<(), String> {
        Err("this driver does not record, so cut() and resume() do nothing here".into())
    }
}

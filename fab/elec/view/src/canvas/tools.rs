//! A canvas's tool strip (E10 2): the armed-tool model every 2D pane shares, and the strip's
//! place over the pane. Each pane lists its own tools; nothing here knows circuits or boards.
//!
//! Select is the rest state (the tool type's default). A click on a tool's button, or its key,
//! arms the tool for one gesture, and once that gesture is done the pane is back on Select. A
//! double click on the button, or the key pressed again while the tool is armed, keeps it armed
//! until Escape, which always goes one step out. Under Select the pane's plain gestures stay what
//! they were, so nobody has to use the strip.
//!
//! The host gives a strip its look: a pane's `tools` slot holds a widget whose parts the pane
//! names (its own module lists them). A pointer press, hover or scroll over a shown part is the
//! strip's, never the canvas's ([`over_strip`]).
//!
//! Cost: per pointer event one rectangle test per named part; per frame the strip's own draw.

use makepad_widgets_core::*;

/// Two clicks on one tool's button within this many seconds keep the tool armed.
pub const DOUBLE_CLICK_S: f64 = 0.4;

/// The strip's inset from the pane's top left corner, in points.
pub const STRIP_INSET: f64 = 10.0;
/// The corner a Fab Electronics area keeps for its editor chip (the shell's notch over the
/// area's top left, a tab bar's left end in size), points: nothing of a pane's own sits under it.
pub const AREA_NOTCH: DVec2 = DVec2 { x: 38.0, y: 28.0 };
/// The strip's top inset: 8 points under the area's notch.
pub const STRIP_TOP: f64 = AREA_NOTCH.y + 8.0;

/// The tool armed now, and whether it stays armed after its gesture. `T::default()` is Select.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
pub struct Armed<T> {
    pub tool: T,
    pub kept: bool,
}

impl<T: Copy + Eq + Default> Armed<T> {
    /// A click on `tool`'s button; `double` when it follows a click on the same button within
    /// [`DOUBLE_CLICK_S`] ([`ToolClicks`]).
    pub fn click(self, tool: T, double: bool) -> Self {
        if tool == T::default() {
            return Self::default();
        }
        Armed { tool, kept: double || self.tool == tool && self.kept }
    }

    /// `tool`'s key: it arms the tool, and pressed again while the tool is armed keeps it.
    pub fn key(self, tool: T) -> Self {
        if tool == T::default() {
            return Self::default();
        }
        Armed { tool, kept: self.tool == tool }
    }

    /// The armed tool did its one thing: back to Select unless it is kept.
    pub fn done(self) -> Self {
        if self.kept {
            self
        } else {
            Self::default()
        }
    }

    /// Escape: out of the tool, kept or not.
    pub fn escape(self) -> Self {
        Self::default()
    }

    /// Whether a tool other than Select is armed.
    pub fn armed(self) -> bool {
        self.tool != T::default()
    }
}

/// The last click on a tool's button, to tell a double click.
#[derive(Copy, Clone, Debug, Default)]
pub struct ToolClicks<T> {
    last: Option<(T, f64)>,
}

impl<T: Copy + Eq> ToolClicks<T> {
    /// A click on `tool`'s button at `now` (seconds): whether it doubles the click before.
    pub fn click(&mut self, tool: T, now: f64) -> bool {
        let double = self.last.is_some_and(|(last, at)| last == tool && now - at < DOUBLE_CLICK_S);
        self.last = Some((tool, now));
        double
    }
}

/// Whether a pointer press, hover or scroll is over one of the shown `parts` of a strip or a
/// status bar; the moves of a press the canvas holds (`pressed`) stay the canvas's. O(parts).
pub fn over_strip(cx: &Cx, tools: &WidgetRef, parts: &[&[LiveId]], event: &Event, pressed: bool) -> bool {
    let abs = match event {
        Event::MouseDown(e) => e.abs,
        Event::MouseMove(e) if !pressed => e.abs,
        Event::Scroll(e) => e.abs,
        _ => return false,
    };
    parts.iter().any(|&part| {
        let part = tools.widget(cx, part);
        part.visible() && part.area().rect(cx).contains(abs)
    })
}

/// Draws a pane's status bar along its bottom edge, the pane's whole width (`rect` is the pane's),
/// as the pane's child. Answers the height it takes (0 when it is hidden), so the strip stops
/// above it.
pub fn draw_bar(cx: &mut Cx2d, scope: &mut Scope, pane: WidgetUid, bar: &WidgetRef, rect: Rect) -> f64 {
    if !bar.visible() {
        return 0.0;
    }
    cx.widget_tree_insert_child(pane, live_id!(status), bar.clone());
    let mut walk = bar.walk(cx);
    let height = match walk.height { Size::Fixed(height) => height, _ => 0.0 };
    walk = walk.with_abs_pos(rect.pos + dvec2(0.0, rect.size.y - height));
    walk.width = Size::Fixed(rect.size.x);
    bar.draw_walk_all(cx, scope, walk);
    height
}

/// Draws a floating panel of the pane (a list over the sheet) at `rect`, in window coordinates,
/// as the pane's child `id`. Nothing when it is hidden.
pub fn draw_float(cx: &mut Cx2d, scope: &mut Scope, pane: WidgetUid, id: LiveId, panel: &WidgetRef, rect: Rect) {
    if !panel.visible() {
        return;
    }
    cx.widget_tree_insert_child(pane, id, panel.clone());
    let mut walk = panel.walk(cx).with_abs_pos(rect.pos);
    walk.width = Size::Fixed(rect.size.x);
    walk.height = Size::Fixed(rect.size.y);
    panel.draw_walk_all(cx, scope, walk);
}

/// Draws the strip over the pane at its top left (`rect` is the pane's), under the area's notch,
/// as the pane's child. A strip whose height is Fill runs down the pane to its inset from the
/// bottom. Returns the strip as drawn (empty while hidden): a pane keeps it for what it draws
/// before the strip on its next draw, as an area reads as empty once its draw list records again.
pub fn draw_strip(cx: &mut Cx2d, scope: &mut Scope, pane: WidgetUid, tools: &WidgetRef, rect: Rect) -> Rect {
    if !tools.visible() {
        return Rect::default();
    }
    cx.widget_tree_insert_child(pane, live_id!(tools), tools.clone());
    let mut walk = tools.walk(cx).with_abs_pos(rect.pos + dvec2(STRIP_INSET, STRIP_TOP));
    if walk.height.is_fill() {
        walk.height = Size::Fixed((rect.size.y - STRIP_TOP - STRIP_INSET).max(0.0));
    }
    tools.draw_walk_all(cx, scope, walk);
    tools.widget(cx, ids!(strip)).area().rect(cx)
}

#[cfg(test)]
mod tests {
    use super::{Armed, ToolClicks, DOUBLE_CLICK_S};

    #[derive(Copy, Clone, PartialEq, Eq, Debug, Default)]
    enum Tool {
        #[default]
        Select,
        Wire,
        Probe,
    }

    #[test]
    fn a_tool_does_one_gesture_unless_kept_and_escape_always_leaves_it() {
        let wire = Armed::default().click(Tool::Wire, false);
        assert_eq!(wire, Armed { tool: Tool::Wire, kept: false }, "a click arms the tool for one gesture");
        assert!(wire.armed() && !Armed::<Tool>::default().armed(), "Select is no armed tool");
        assert_eq!(wire.done(), Armed::default(), "its gesture done, the pane is back on Select");
        let kept = wire.click(Tool::Wire, true);
        assert!(kept.kept, "a double click keeps it");
        assert_eq!(kept.done(), kept, "a kept tool stays armed after its gesture");
        assert_eq!(kept.click(Tool::Wire, false), kept, "a single click on the kept tool leaves it kept");
        assert_eq!(kept.click(Tool::Probe, false), Armed { tool: Tool::Probe, kept: false }, "another tool's click arms that one, once");
        assert_eq!(kept.escape(), Armed::default(), "Escape leaves a kept tool too");
        assert_eq!(kept.click(Tool::Select, true), Armed::default(), "Select is the rest state, never kept");

        let keyed = Armed::default().key(Tool::Wire);
        assert_eq!(keyed, wire, "the key arms like a click");
        assert!(keyed.key(Tool::Wire).kept, "the key pressed again while armed keeps the tool");
        assert_eq!(keyed.key(Tool::Select), Armed::default(), "Select's key is the way back");

        let mut clicks = ToolClicks::default();
        assert!(!clicks.click(Tool::Wire, 1.0), "a first click is single");
        assert!(clicks.click(Tool::Wire, 1.0 + DOUBLE_CLICK_S * 0.5), "the same button again soon is a double click");
        assert!(!clicks.click(Tool::Probe, 1.0 + DOUBLE_CLICK_S * 0.6), "another button's click is single");
        assert!(!clicks.click(Tool::Probe, 1.0 + DOUBLE_CLICK_S * 2.0), "the same button late is single");
    }
}

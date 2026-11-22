//! The F3 command palette: fuzzy search over the commands the app can
//! actually run, Enter (or a click) fires the highlighted one.
//!
//! **The list is the inventory, not a wish list.** Every row here maps to a
//! string `main.rs::App::run_command` already understands, so a row can never
//! be a dead end — if `run_command` grows a verb, it gets a row; if it loses
//! one, the row goes with it. After them, one "Example: …" row per demo of the
//! app's demo list (`AppState::plates`), each opening that demo. Ranking is a subsequence match with a bonus for
//! prefix and word-start hits, which is what makes "fa" find "Frame All"
//! first.

use crate::api::*;
use crate::ui::keymap::{self, ShortcutId as Key};
use makepad_widgets_core::*;

/// `(command string handed to run_command, label, keymap entry)`.
pub const COMMANDS: &[(&str, &str, Option<Key>)] = &[
    ("frame all", "Frame All", Some(Key::FrameAll)),
    ("frame selected", "Frame Selected", Some(Key::FrameSelected)),
    ("front view", "View: Front", Some(Key::Front)),
    ("right view", "View: Right", Some(Key::Right)),
    ("top view", "View: Top", Some(Key::Top)),
    ("isometric", "View: Isometric", Some(Key::Isometric)),
    ("toggle ortho", "Toggle Orthographic", Some(Key::Ortho)),
    ("solid", "Shading: Solid", None),
    ("realtime", "Shading: Realtime", None),
    ("hide selected", "Hide Selected", Some(Key::Hide)),
    ("isolate selected", "Isolate Selected", Some(Key::Isolate)),
    ("unhide all", "Unhide All", Some(Key::Unhide)),
    ("measure distance", "Measure Distance", Some(Key::Measure)),
    ("measure area", "Measure Area", None),
    ("measure angle", "Measure Angle", None),
    ("measure edge", "Measure Edge", None),
    ("measure radius", "Measure Radius", None),
    ("measure faces", "Measure Faces", None),
    ("undo", "Undo", Some(Key::Undo)),
    ("redo", "Redo", Some(Key::Redo)),
    ("undo ai turn", "Undo the AI's Last Turn", None),
    ("new plate", "New Plate", Some(Key::New)),
    ("open", "Open…", Some(Key::Open)),
    ("import", "Import…", Some(Key::Import)),
    ("browse plates", "Browse Plates…", Some(Key::Plates)),
    ("save plate", "Save Plate", Some(Key::Save)),
    ("export 3mf", "Export 3MF", Some(Key::Export3mf)),
    ("export stl", "Export STL", Some(Key::ExportStl)),
    ("slice", "Slice the Plate", Some(Key::Slice)),
    ("slice view", "Slice Render Mode", Some(Key::SliceView)),
    ("show supports", "Show or Hide the Supports", Some(Key::ShowSupports)),
    ("show travel", "Show or Hide Travel", None),
    ("show parts", "Show or Hide the Parts Under the Toolpaths", None),
    ("cut at layer", "Cut the Parts Among the Toolpaths at the Layer", None),
    ("toggle zones", "Nozzle Zones", None),
    ("toggle code", "Show or Hide the Code", Some(Key::Code)),
    ("insert selector", "Insert the Picks' Selectors in the Code", None),
    ("drop to plate", "Drop to Plate", None),
    ("arrange plate", "Arrange Plate", None),
    ("toggle overhangs", "Overhangs", None),
    ("export print", "Export the Print (G-code)", None),
    ("toggle chat", "Ask the AI", Some(Key::Chat)),
    ("keymap", "Keymap Help", Some(Key::Help)),
    ("about", "About Fab", None),
    ("quit", "Quit", Some(Key::Quit)),
];

/// Subsequence score: `None` when `needle` does not fit in `hay` in order.
/// Higher is better; consecutive runs and word starts score more.
fn score(hay: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let h: Vec<char> = hay.to_lowercase().chars().collect();
    let n: Vec<char> = needle.to_lowercase().chars().collect();
    let mut hi = 0usize;
    let mut total = 0i32;
    let mut run = 0i32;
    for c in n.iter() {
        let mut found = None;
        while hi < h.len() {
            if h[hi] == *c {
                found = Some(hi);
                break;
            }
            hi += 1;
        }
        let at = found?;
        let word_start = at == 0 || h[at - 1] == ' ' || h[at - 1] == ':';
        run = if run > 0 { run + 1 } else { 1 };
        total += 4 + run * 2 + if word_start { 6 } else { 0 } - (at as i32).min(12);
        hi = at + 1;
    }
    Some(total)
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    // A `PortalList` cannot live inside a `Modal`: `Modal::draw_walk` calls
    // `content.draw_all()`, which swallows the draw steps a list needs to be
    // filled. A fixed pool of rows is the honest way to put a list in a modal.
    let PaletteRow = View{
        width: Fill
        height: 24
        flow: Right
        align: Align{x: 0.0 y: 0.5}
        padding: Inset{left: 8 right: 8 top: 0 bottom: 0}
        spacing: 8
        cursor: MouseCursor.Hand
        show_bg: true
        draw_bg +: {
            hover: instance(0.0)
            active: instance(0.0)
            pixel: fn() {
                let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                sdf.box(1.0, 0.0, self.rect_size.x - 2.0, self.rect_size.y, fab.radius)
                sdf.fill(vec4(fab.color_menu_row_hover.xyz, max(self.hover * 0.6, self.active)))
                return sdf.result
            }
        }
        animator: Animator{
            hover: {
                default: @off
                off: AnimatorState{
                    from: {all: Forward {duration: fab.anim_fast}}
                    apply: { draw_bg: {hover: 0.0} }
                }
                on: AnimatorState{
                    from: {all: Snap}
                    apply: { draw_bg: {hover: 1.0} }
                }
            }
        }
        label := mod.widgets.FabLabel{ width: Fill text: "" }
        shortcut := mod.widgets.FabLabelSmall{
            text: ""
            draw_text +: { ink_centered: true color: fab.color_text_muted }
        }
    }

    mod.widgets.FabCommandPaletteBase = #(FabCommandPalette::register_widget(vm))
    mod.widgets.FabCommandPalette = set_type_default() do mod.widgets.FabCommandPaletteBase{
        width: Fill
        height: Fill
        modal := Modal{
            content +: {
                width: 520
                height: Fit
                View{
                    width: Fill
                    height: Fit
                    flow: Down
                    padding: 8
                    spacing: 6
                    show_bg: true
                    draw_bg +: {
                        pixel: fn() {
                            let sdf = Sdf2d.viewport(self.pos * self.rect_size)
                            sdf.box(0.5, 0.5, self.rect_size.x - 1.0, self.rect_size.y - 1.0, fab.radius_lg)
                            sdf.fill_keep(fab.color_popover)
                            sdf.stroke(fab.color_popover_border, 1.0)
                            return sdf.result
                        }
                    }
                    search := TextInput{
                        width: Fill
                        height: 26
                        empty_text: "Search commands…"
                        draw_bg +: {
                            color: fab.color_input
                            border_radius: fab.radius
                        }
                        draw_text +: {
                            color: fab.color_text
                            ink_centered: true
                            text_style: theme.font_regular{ font_size: fab.font_size_header }
                        }
                    }
                    rows := View{
                        width: Fill
                        height: Fit
                        flow: Down
                        spacing: 1
                        r0 := PaletteRow{ visible: false }
                        r1 := PaletteRow{ visible: false }
                        r2 := PaletteRow{ visible: false }
                        r3 := PaletteRow{ visible: false }
                        r4 := PaletteRow{ visible: false }
                        r5 := PaletteRow{ visible: false }
                        r6 := PaletteRow{ visible: false }
                        r7 := PaletteRow{ visible: false }
                        r8 := PaletteRow{ visible: false }
                        r9 := PaletteRow{ visible: false }
                        r10 := PaletteRow{ visible: false }
                        r11 := PaletteRow{ visible: false }
                        empty := mod.widgets.FabLabelMuted{
                            visible: false
                            margin: Inset{left: 8 top: 6 bottom: 6}
                            text: "No command matches"
                        }
                    }
                    hint := mod.widgets.FabLabelMuted{
                        text: #(format!("{} / {} to choose · {} to run · {} to close", keymap::shortcut(Key::PaletteUp), keymap::shortcut(Key::PaletteDown), keymap::shortcut(Key::PaletteRun), keymap::shortcut(Key::PaletteClose)))
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabCommandPalette {
    #[deref]
    view: View,
    #[rust]
    shown: bool,
    #[rust]
    query: String,
    /// Indices into the rows: below `COMMANDS.len()` a command, past it a demo.
    #[rust]
    hits: Vec<usize>,
    /// The demos' rows: key and label, from the app's demo list.
    #[rust]
    demos: Vec<(String, String)>,
    #[rust]
    cursor: usize,
    #[rust]
    top: usize,
    #[rust]
    want_focus: bool,
}

/// How many rows the pool holds.
const SLOTS: usize = 12;

fn slot_ids() -> [&'static [LiveId]; SLOTS] {
    [
        ids!(modal.rows.r0),
        ids!(modal.rows.r1),
        ids!(modal.rows.r2),
        ids!(modal.rows.r3),
        ids!(modal.rows.r4),
        ids!(modal.rows.r5),
        ids!(modal.rows.r6),
        ids!(modal.rows.r7),
        ids!(modal.rows.r8),
        ids!(modal.rows.r9),
        ids!(modal.rows.r10),
        ids!(modal.rows.r11),
    ]
}

impl FabCommandPalette {
    /// The demo rows follow the app's demo list.
    fn sync_demos(&mut self, state: &AppState) {
        let demos: Vec<(String, String)> = state.plates.demos.iter().map(|d| (d.id.clone(), format!("Example: {}", d.name))).collect();
        if demos != self.demos {
            self.demos = demos;
            self.refilter();
        }
    }

    /// A row's label and shortcut.
    fn row(&self, i: usize) -> (&str, Option<Key>) {
        match COMMANDS.get(i) {
            Some((_, label, key)) => (label, *key),
            None => {
                let (id, label) = &self.demos[i - COMMANDS.len()];
                (label, (id == "demo").then_some(Key::Demo))
            }
        }
    }

    fn refilter(&mut self) {
        let q = self.query.trim().to_string();
        let mut scored: Vec<(i32, usize)> = (0..COMMANDS.len() + self.demos.len())
            .filter_map(|i| score(self.row(i).0, &q).map(|s| (s, i)))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        self.hits = scored.into_iter().map(|(_, i)| i).collect();
        self.cursor = 0;
        self.top = 0;
    }

    /// Keep the cursor inside the visible window.
    fn scroll_into_view(&mut self) {
        if self.cursor < self.top {
            self.top = self.cursor;
        } else if self.cursor >= self.top + SLOTS {
            self.top = self.cursor + 1 - SLOTS;
        }
    }

    fn run(&mut self, cx: &mut Cx) {
        if let Some(&i) = self.hits.get(self.cursor) {
            cx.action(ShellAction::ToggleCommandPalette);
            match COMMANDS.get(i) {
                Some((name, _, _)) => cx.action(ShellAction::Command(name.to_string())),
                None => cx.action(ShellAction::OpenDemo(self.demos[i - COMMANDS.len()].0.clone())),
            }
        }
    }
}

impl Widget for FabCommandPalette {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if !self.shown {
            return;
        }
        if let Event::KeyDown(ke) = event {
            match ke.key_code {
                KeyCode::ArrowDown => {
                    if !self.hits.is_empty() {
                        self.cursor = (self.cursor + 1) % self.hits.len();
                        self.scroll_into_view();
                        self.view.redraw(cx);
                    }
                }
                KeyCode::ArrowUp => {
                    if !self.hits.is_empty() {
                        self.cursor = (self.cursor + self.hits.len() - 1) % self.hits.len();
                        self.scroll_into_view();
                        self.view.redraw(cx);
                    }
                }
                KeyCode::ReturnKey => {
                    self.run(cx);
                    return;
                }
                _ => {}
            }
        }
        let Event::Actions(actions) = event else {
            return;
        };
        let modal = self.view.modal(cx, ids!(modal));
        if modal.dismissed(actions) {
            cx.action(ShellAction::ToggleCommandPalette);
            return;
        }
        if let Some(state) = scope.data.get::<AppState>() {
            self.sync_demos(state);
        }
        let search = self.view.text_input(cx, ids!(modal.search));
        if let Some(text) = search.changed(actions) {
            self.query = text;
            self.refilter();
            self.view.redraw(cx);
        }
        // Enter runs from the key handler above; the field's own return is the same press
        for (slot, id) in slot_ids().iter().enumerate() {
            // A press taken away (a list or the host took the finger) runs nothing.
            if self.view.view(cx, id).finger_up(actions).is_some_and(|e| !e.cancelled) {
                self.cursor = self.top + slot;
                self.run(cx);
                return;
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get_mut::<AppState>() {
            self.sync_demos(state);
            if state.ui.command_palette_open != self.shown {
                self.shown = state.ui.command_palette_open;
                let modal = self.view.modal(cx, ids!(modal));
                if self.shown {
                    self.query.clear();
                    self.refilter();
                    modal.open(cx);
                    self.view.text_input(cx, ids!(modal.search)).set_text(cx, "");
                    self.want_focus = true;
                } else {
                    modal.close(cx);
                }
            }
        }
        if self.shown {
            if self.hits.is_empty() && self.query.is_empty() {
                self.refilter();
            }
            let hits = self.hits.clone();
            for (slot, id) in slot_ids().iter().enumerate() {
                let row = self.view.view(cx, id);
                match hits.get(self.top + slot) {
                    Some(&ci) => {
                        let (label, key) = self.row(ci);
                        let (label, key) = (label.to_string(), key);
                        row.set_visible(cx, true);
                        row.label(cx, ids!(label)).set_text(cx, &label);
                        row.label(cx, ids!(shortcut)).set_text(cx, &key.map(keymap::shortcut).unwrap_or_default());
                        let a: f32 = if self.top + slot == self.cursor { 1.0 } else { 0.0 };
                        if let Some(mut row) = row.borrow_mut() {
                            row.draw_bg.set_dyn_instance(cx, live_id!(active), &[a]);
                        }
                    }
                    None => row.set_visible(cx, false),
                }
            }
            self.view
                .widget(cx, ids!(modal.rows.empty))
                .set_visible(cx, hits.is_empty());
        }
        let r = self.view.draw_walk(cx, scope, walk);
        // The field only has an area once it has been drawn, so focus is taken
        // on the frame after the modal opened.
        if self.want_focus {
            self.want_focus = false;
            self.view.text_input(cx, ids!(modal.search)).take_key_focus(cx);
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranks_frame_all_first_for_fa() {
        let mut best = None;
        let mut best_score = i32::MIN;
        for (_, label, _) in COMMANDS {
            if let Some(s) = score(label, "fa") {
                if s > best_score {
                    best_score = s;
                    best = Some(*label);
                }
            }
        }
        assert_eq!(best, Some("Frame All"));
    }

    #[test]
    fn non_subsequence_does_not_match() {
        assert!(score("Frame All", "zzz").is_none());
    }
}

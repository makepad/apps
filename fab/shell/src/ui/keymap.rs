//! Keymap help (F1 / Help menu), generated from one table so the
//! tooltips, the menus and this panel can never disagree.
//!
//! It is drawn with our own rows rather than `Markdown` on purpose: the
//! markdown path pulled the mono font in mid-frame and the shared glyph atlas
//! came back with holes — whole words vanished from every panel in the app.
//! A `PortalList` of two labels and a key cap has no such appetite.

use crate::api::*;
use makepad_widgets_core::*;

/// Where a binding is routed; context can further restrict the active tool or panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyScope {
    Global,
    Viewport,
    ContextFocus,
}

pub struct KeymapEntry {
    pub id: ShortcutId,
    pub chord: &'static str,
    pub action: &'static str,
    pub context: &'static str,
    pub scope: KeyScope,
}

macro_rules! keymap {
    ($($id:ident => ($chord:literal, $action:literal, $context:literal, $scope:ident)),* $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ShortcutId { $($id),* }

        pub const KEYMAP: &[KeymapEntry] = &[
            $(KeymapEntry { id: ShortcutId::$id, chord: $chord, action: $action, context: $context, scope: KeyScope::$scope }),*
        ];
    };
}

keymap! {
    Orbit => ("LMB drag", "Orbit", "Navigation", Viewport),
    Pan => ("RMB drag", "Pan (the Select tool; other tools keep RMB for their own use)", "Navigation", Viewport),
    Zoom => ("Wheel", "Zoom to cursor", "Navigation", Viewport),
    OrbitAny => ("Shift+LMB drag", "Orbit in any tool", "Navigation", Viewport),
    Dolly => ("Cmd/Ctrl+LMB drag", "Dolly", "Navigation", Viewport),
    PanEmulated => ("Alt+LMB drag", "Pan", "Navigation", Viewport),
    SelectPart => ("LMB click", "Select a part", "Navigation", Viewport),
    ExtendSelection => ("Shift+LMB click", "Extend selection", "Navigation", Viewport),
    PartMenu => ("RMB click", "Part menu", "Navigation", Viewport),
    Pivot => ("Double-click", "Set orbit pivot", "Navigation", Viewport),
    Front => ("Numpad 1", "Front", "Navigation", Viewport),
    Back => ("Ctrl+Numpad 1", "Back", "Navigation", Viewport),
    Right => ("Numpad 3", "Right", "Navigation", Viewport),
    Left => ("Ctrl+Numpad 3", "Left", "Navigation", Viewport),
    Top => ("Numpad 7", "Top", "Navigation", Viewport),
    Bottom => ("Ctrl+Numpad 7", "Bottom", "Navigation", Viewport),
    Isometric => ("Numpad 9", "Isometric", "Navigation", Viewport),
    Ortho => ("Numpad 5 / 5", "Orthographic / Perspective", "Navigation", Viewport),
    OrbitStep => ("Numpad 2 / Numpad 4 / Numpad 6 / Numpad 8", "Orbit 15°", "Navigation", Viewport),
    FrameAll => ("Home", "Frame all", "Navigation", Viewport),
    FrameSelected => (".", "Frame selected", "Navigation", Viewport),
    OrbitSelection => ("O", "Orbit Around Selection", "Navigation", Viewport),
    Select => ("Q", "Select", "Design / Monitor", Global),
    Move => ("G", "Move on the plate", "Design / Monitor", Global),
    Rotate => ("R", "Turn about Z", "Design / Monitor (outside Sculpt)", Global),
    Scale => ("S", "Scale", "Design / Monitor", Global),
    Shape => ("E", "Edit shape: drag the part's handles", "Design / Monitor", Global),
    Paint => ("B", "Paint", "Design / Monitor", Global),
    DuplicateLinked => ("Cmd/Ctrl+D", "Duplicate (linked)", "Parts list / viewport (Design / Monitor)", ContextFocus),
    DuplicateCopy => ("Cmd/Ctrl+Shift+D", "Duplicate as copy", "Parts list / viewport (Design / Monitor)", ContextFocus),
    Hide => ("H", "Hide selected", "Design / Monitor", Global),
    Isolate => ("Shift+H", "Isolate selected", "Design / Monitor", Global),
    Unhide => ("Alt+H", "Unhide all", "Design / Monitor", Global),
    Solo => ("/", "Solo the selection", "Design / Monitor", Global),
    Xray => ("Alt+Z", "X-ray", "Design / Monitor", Global),
    Measure => ("M", "Measure (cycle distance, area, angle, edge, radius, faces)", "Design / Monitor", Global),
    Section => ("C", "Clip: a cross-section through the parts", "Design / Monitor (outside Sculpt)", Global),
    SectionBox => ("Shift+C", "Section box", "Design / Monitor", Global),
    SectionClear => ("Alt+C", "Clear section", "Design / Monitor", Global),
    SliceView => ("L", "Slice render mode on / off", "Design / Monitor (outside Sculpt)", Global),
    Delete => ("Delete / Backspace", "Delete the selected measurement or parts", "Parts list / viewport", ContextFocus),
    Rename => ("F2", "Rename a part, script node or measurement", "Parts list", ContextFocus),
    RenameClick => ("Double-click", "Rename a part, script node or measurement", "Parts list", Global),
    SectionFlip => ("F", "Flip the section", "Section tool", Global),
    SectionPlace => ("LMB click", "Face to cut", "Section tool", Viewport),
    SectionOffset => ("Drag handle", "Offset section", "Section tool", Viewport),
    MeasurePlace => ("LMB click", "Place point / pick edge or face", "Measure tool", Viewport),
    MeasureClose => ("Enter / RMB click", "Close the area loop", "Measure tool", Viewport),
    MeasureUndo => ("Backspace", "Remove the last point", "Measure tool (parts list / viewport)", ContextFocus),
    NoTreesBox => ("LMB drag", "No trees here: a box on the bed", "No-trees tool", Viewport),
    NoTreesDisc => ("Alt+LMB drag", "No trees here: a disc", "No-trees tool", Viewport),
    SettingsHereBox => ("LMB drag", "Print settings here: a box across the model (Sliced: on the shown layer)", "Settings-here tool", Viewport),
    SettingsHereSphere => ("Alt+LMB drag", "Print settings here: a sphere", "Settings-here tool", Viewport),
    Cancel => ("Esc", "Cancel / back to Select", "Tools", Viewport),
    PaintBrush => ("B", "Paint", "Paint tool", Global),
    PaintErase => ("X", "Erase", "Paint tool", Global),
    PaintFeature => ("F", "Fill feature", "Paint tool", Global),
    PaintPart => ("P", "Fill part", "Paint tool", Global),
    PaintFlood => ("N", "Smooth fill", "Paint tool", Global),
    PaintPattern => ("V", "Pattern fill", "Paint tool", Global),
    PaintSmaller => ("[", "Smaller brush diameter", "Paint tool", Global),
    PaintLarger => ("]", "Larger brush diameter", "Paint tool", Global),
    PaintInvert => ("Alt", "Flip paint and erase while held", "Paint tool", Viewport),
    PaintExtend => ("Shift+LMB click", "Extend feature fill", "Paint tool", Viewport),
    PaintSizeScroll => ("Shift+Wheel", "Brush diameter", "Paint tool", Viewport),
    Sculpt => ("K", "Sculpt", "Design", Global),
    SculptRaise => ("R", "Raise", "Sculpt tool", Global),
    SculptLower => ("L", "Lower", "Sculpt tool", Global),
    SculptCrease => ("C", "Crease", "Sculpt tool", Global),
    SculptSmaller => ("[", "Smaller brush diameter", "Sculpt tool", Global),
    SculptLarger => ("]", "Larger brush diameter", "Sculpt tool", Global),
    SculptInvert => ("Alt", "Flip raise and lower while held", "Sculpt tool", Viewport),
    SculptSizeScroll => ("Shift+Wheel", "Brush diameter", "Sculpt tool", Viewport),
    SculptStrength => ("Drag / Wheel", "Strength from 0.1 to 1.0", "Sculpt tool", Viewport),
    Force => ("F", "Force: fixtures and loads on the studied part", "Design", Global),
    ForceFixed => ("X", "Fixed: click where the part is held", "Force tool", Global),
    ForceLoad => ("F", "Force: click where it pushes (N)", "Force tool", Global),
    ForcePressure => ("P", "Pressure: click the face it presses on (MPa)", "Force tool", Global),
    ForceTorque => ("T", "Torque: click the hole or face it turns (N mm)", "Force tool", Global),
    ForceRemove => ("Delete / Backspace", "Remove the selected load", "Force tool", Global),
    ForceFlip => ("Alt", "The other reading: a hole's wall as a face, a face as a hole", "Force tool", Viewport),
    PrintPlay => ("Space", "Play / pause the print", "Slice", Global),
    SupportsDeleteBranch => ("Delete / Backspace", "Take the picked branch out of the supports", "Slice", ContextFocus),
    ShowSupports => ("U", "Show supports", "Slice", Global),
    MechGrab => ("G", "Grab", "Mechanics", Global),
    MechThrow => ("T", "Throw", "Mechanics", Global),
    MechPin => ("P", "Pin", "Mechanics", Global),
    MechDangle => ("D", "Dangle", "Mechanics", Global),
    MechPoke => ("K", "Poke", "Mechanics", Global),
    MechPlay => ("Space", "Pause / resume", "Mechanics", Global),
    MechReset => ("R", "Reset scene", "Mechanics", Global),
    MechUse => ("LMB click", "Use the tool and select the part", "Mechanics", Viewport),
    MechMenu => ("RMB click", "Action menu: drop, hang, pin, throw, reset, freeze", "Mechanics", Viewport),
    MechOrbit => ("LMB drag / Shift+LMB drag / MMB drag", "Orbit (bare drag off a part)", "Mechanics", Viewport),
    MechDepth => ("Wheel", "Move the held point nearer or farther", "Mechanics", Viewport),
    ValueDrag => ("Drag", "Change a value", "Panels", Global),
    ValueType => ("Click", "Type a value", "Panels", Global),
    ValueSnap => ("Ctrl", "Snap to increment while dragging", "Panels", Global),
    ValueFine => ("Shift", "Fine adjust while dragging", "Panels", Global),
    CodeScrub => ("Alt+LMB drag", "Scrub a number", "Code panel", Global),
    Code => ("`", "Show or hide the code", "Design", Global),
    Workspaces => ("Cmd/Ctrl+1 … 9", "The caption's first nine workspaces, in its order", "App", Global),
    PrevWorkspace => ("Ctrl+PageUp", "The workspace left of the shown one", "App", Global),
    NextWorkspace => ("Ctrl+PageDown", "The workspace right of the shown one", "App", Global),
    Undo => ("Cmd/Ctrl+Z", "Undo", "App", Global),
    Redo => ("Cmd/Ctrl+Shift+Z", "Redo", "App", Global),
    New => ("Cmd/Ctrl+N", "New plate", "App", Global),
    Save => ("Cmd/Ctrl+S", "Save the plate", "App", Global),
    Export3mf => ("Cmd/Ctrl+E", "Export 3MF", "App", Global),
    ExportStl => ("Cmd/Ctrl+Shift+E", "Export STL", "App", Global),
    Slice => ("Cmd/Ctrl+R", "Slice the plate", "App", Global),
    Chat => ("Cmd/Ctrl+K", "Ask the AI", "App", Global),
    Palette => ("F3", "Command palette", "App", Global),
    Perf => ("Shift+F3", "Performance graph", "App", Global),
    Open => ("Cmd/Ctrl+O", "Open", "App", Global),
    Import => ("Cmd/Ctrl+I", "Import meshes", "App", Global),
    Plates => ("Cmd/Ctrl+Shift+M", "Browse plates", "App", Global),
    Demo => ("Cmd/Ctrl+Shift+O", "Open the demo plate example", "App", Global),
    Help => ("F1", "Keymap reference", "App", Global),
    Quit => ("Cmd/Ctrl+Q", "Quit", "App", Global),
    PaletteUp => ("Up", "Previous command", "Command palette", Global),
    PaletteDown => ("Down", "Next command", "Command palette", Global),
    PaletteRun => ("Enter", "Run command", "Command palette", Global),
    PaletteClose => ("Esc", "Close command palette", "Command palette", Global),
}

pub fn entry(id: ShortcutId) -> &'static KeymapEntry {
    &KEYMAP[id as usize]
}

/// Each alternative is a complete chord; the primary modifier is always `Cmd/Ctrl`.
pub fn shortcut(id: ShortcutId) -> String {
    entry(id).chord.split(" / ").map(makepad_widgets_core::kbd::shortcut_label).collect::<Vec<_>>().join(" / ")
}

pub fn tooltip(id: ShortcutId) -> String {
    format!("{} ({})", entry(id).action, shortcut(id))
}

/// The rows the panel draws: section headers interleaved with entries.
enum HelpRow {
    Section(String),
    Entry(ShortcutId),
}

fn help_rows() -> Vec<HelpRow> {
    let mut out = Vec::new();
    let mut last = None;
    for e in KEYMAP {
        if last != Some((e.context, e.scope)) {
            last = Some((e.context, e.scope));
            let scope = match e.scope { KeyScope::Global => "Global", KeyScope::Viewport => "Viewport focus", KeyScope::ContextFocus => "Focus in this context" };
            out.push(HelpRow::Section(format!("{} · {scope}", e.context)));
        }
        out.push(HelpRow::Entry(e.id));
    }
    out
}

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabKeymapHelpBase = #(FabKeymapHelp::register_widget(vm))
    mod.widgets.FabKeymapHelp = set_type_default() do mod.widgets.FabKeymapHelpBase{
        width: Fill
        height: Fill
        modal := Modal{
            content +: {
                width: 620
                height: Fit
                View{
                    width: Fill
                    height: Fit
                    flow: Down
                    padding: 14
                    spacing: 8
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
                    title_row := View{
                        width: Fill
                        height: Fit
                        flow: Right
                        align: Align{x: 0.0 y: 0.5}
                        spacing: 8
                        mod.widgets.FabTitleLabel{ text: "Keymap" }
                        Filler{}
                        mod.widgets.FabLabelMuted{ text: #(format!("{} closes", shortcut(ShortcutId::Help))) }
                    }
                    mod.widgets.FabHr{}
                    list := PortalList{
                        width: Fill
                        height: 520
                        flow: Down
                        auto_tail: false
                        Section := View{
                            width: Fill
                            height: 26
                            flow: Right
                            align: Align{x: 0.0 y: 1.0}
                            padding: Inset{left: 2 right: 2 top: 8 bottom: 2}
                            head := mod.widgets.FabHeaderLabel{ text: "" }
                        }
                        Entry := View{
                            width: Fill
                            height: theme.prop_row_height
                            flow: Right
                            align: Align{x: 0.0 y: 0.5}
                            padding: Inset{left: 2 right: 2 top: 0 bottom: 0}
                            spacing: 10
                            keys := View{
                                width: 190
                                height: Fit
                                flow: Right
                                cap := mod.widgets.FabKeyCap{ cap +: { text: "" } }
                            }
                            what := mod.widgets.FabLabel{ width: Fill text: "" }
                        }
                    }
                    View{
                        width: Fill
                        height: Fit
                        flow: Right
                        align: Align{x: 0.0 y: 0.5}
                        Filler{}
                        close := mod.widgets.FabButton{ text: "Close" }
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabKeymapHelp {
    #[rust]
    rows: Vec<HelpRow>,
    #[deref]
    view: View,
    #[rust]
    shown: bool,
}

impl Widget for FabKeymapHelp {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        if let Event::Actions(actions) = event {
            let modal = self.view.modal(cx, ids!(modal));
            if modal.dismissed(actions) || self.view.button(cx, ids!(modal.close)).clicked(actions) {
                cx.action(ShellAction::ShowKeymapHelp(false));
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get_mut::<AppState>() {
            if state.ui.keymap_help_open != self.shown {
                self.shown = state.ui.keymap_help_open;
                let modal = self.view.modal(cx, ids!(modal));
                if self.shown {
                    if self.rows.is_empty() { self.rows = help_rows(); }
                    modal.open(cx);
                } else {
                    modal.close(cx);
                }
            }
        }
        let rows = &self.rows;
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            let list_ref = step.as_portal_list();
            let Some(mut list) = list_ref.borrow_mut() else {
                continue;
            };
            list.set_item_range(cx, 0, rows.len());
            while let Some(i) = list.next_visible_item(cx) {
                match rows.get(i) {
                    Some(HelpRow::Section(name)) => {
                        let item = list.item(cx, i, live_id!(Section));
                        item.label(cx, ids!(head)).set_text(cx, name);
                        item.draw_all(cx, &mut Scope::empty());
                    }
                    Some(HelpRow::Entry(id)) => {
                        let item = list.item(cx, i, live_id!(Entry));
                        item.label(cx, ids!(keys.cap.cap)).set_text(cx, &shortcut(*id));
                        item.label(cx, ids!(what)).set_text(cx, entry(*id).action);
                        item.draw_all(cx, &mut Scope::empty());
                    }
                    None => {}
                }
            }
        }
        DrawStep::done()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A duplicate binding in the same context is a bug you only notice at
    /// the worst moment, so the table is checked instead.
    #[test]
    fn no_duplicate_binding_per_context() {
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        for e in KEYMAP {
            assert!(seen.insert((e.chord, e.context)), "duplicate binding {} in {}", e.chord, e.context);
        }
    }

    #[test]
    fn duplicate_bindings_are_distinct_plate_actions() {
        assert_eq!(entry(ShortcutId::DuplicateLinked).chord, "Cmd/Ctrl+D");
        assert_eq!(entry(ShortcutId::DuplicateLinked).action, "Duplicate (linked)");
        assert_eq!(entry(ShortcutId::DuplicateCopy).chord, "Cmd/Ctrl+Shift+D");
        assert_eq!(entry(ShortcutId::DuplicateCopy).action, "Duplicate as copy");
        assert_eq!(KEYMAP.iter().filter(|e| e.chord.ends_with("+D")).count(), 2);
    }

    #[test]
    fn every_entry_has_a_section() {
        for row in help_rows() {
            if let HelpRow::Entry(id) = row {
                let e = entry(id);
                assert!(!e.chord.is_empty() && !e.action.is_empty() && !e.context.is_empty());
            }
        }
    }

    #[test]
    fn hints_match_routed_mouse_bindings() {
        let mut cx = Cx::new(Box::new(|_, _| {}));
        for tool in [
            Tool::Select, Tool::Section,
            Tool::Measure(MeasureKind::Distance), Tool::Measure(MeasureKind::Area),
            Tool::Measure(MeasureKind::Angle), Tool::Measure(MeasureKind::EdgeLength),
            Tool::Measure(MeasureKind::Radius), Tool::Measure(MeasureKind::FaceDistance),
        ] {
            let mut state = AppState::default();
            state.tool = tool;
            let mut nav = crate::nav::Navigator::default();
            let mut input = ViewportInput {
                rect: Rect { pos: dvec2(0.0, 0.0), size: dvec2(800.0, 600.0) },
                hit: None,
                kind: ViewportInputKind::PointerDown {
                    button: PointerButton::Secondary,
                    pos: dvec2(100.0, 100.0),
                    mods: KeyModifiers::default(),
                    tap_count: 1,
                },
            };
            assert!(!nav.handle(&mut cx, &input, &mut state).consumed);
            input.kind = ViewportInputKind::PointerMove {
                pos: dvec2(120.0, 100.0),
                delta: dvec2(20.0, 0.0),
                buttons: 2,
                mods: KeyModifiers::default(),
            };
            let pan = nav.handle(&mut cx, &input, &mut state).consumed;
            assert_eq!(pan, tool == Tool::Select, "RMB pan with {tool:?}");
        }

        use ShortcutId as Key;
        assert_eq!(entry(Key::Pan).chord, "RMB drag");
        assert!(entry(Key::Pan).action.contains("Select tool"));
        assert_eq!(entry(Key::Orbit).chord, "LMB drag");
        assert_eq!(entry(Key::OrbitAny).chord, "Shift+LMB drag");
        assert_eq!(entry(Key::Zoom).chord, "Wheel");
        assert_eq!(entry(Key::Dolly).chord, "Cmd/Ctrl+LMB drag");
        assert!(entry(Key::MeasureClose).chord.contains("RMB"));
        assert!(entry(Key::MeasureClose).action.contains("area loop"));
    }

    #[test]
    fn viewport_bindings_are_the_routed_ones() {
        let keys: Vec<&str> = KEYMAP
            .iter()
            .filter(|e| e.context == "Navigation" && e.scope == KeyScope::Viewport)
            .map(|e| e.chord)
            .collect();
        assert!(keys.iter().any(|k| k.contains("LMB drag")), "{keys:?}");
        assert!(keys.iter().any(|k| k.contains("RMB drag")), "{keys:?}");
        assert!(keys.iter().any(|k| *k == "Wheel"), "{keys:?}");
        assert!(!keys.iter().any(|k| k.contains("MMB drag")), "{keys:?}");
    }
}

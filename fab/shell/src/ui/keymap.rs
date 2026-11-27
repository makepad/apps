//! Keymap help (F1 / Help menu), generated from one table so the
//! tooltips, the menus and this panel can never disagree.
//!
//! It is drawn with our own rows rather than `Markdown` on purpose: the
//! markdown path pulled the mono font in mid-frame and the shared glyph atlas
//! came back with holes — whole words vanished from every panel in the app.
//! The rows sit in foldable sections, one per context, built before the modal
//! draws (a list stepped from the modal's draw never got its turn: it showed
//! empty). A rebindable chord is changed on its key cap; the rebindings are
//! kept in the view prefs and applied at the app's door (`remap`).

use crate::api::*;
use makepad_widgets_core::*;
use std::collections::HashMap;

/// Where a binding is routed; context can further restrict the active tool or panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyScope {
    Global,
    Viewport,
    ContextFocus,
}

pub struct KeymapEntry {
    pub id: ShortcutId,
    /// The action's stable name (its `ShortcutId`), what a rebinding is saved under.
    pub name: &'static str,
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
            $(KeymapEntry { id: ShortcutId::$id, name: stringify!($id), chord: $chord, action: $action, context: $context, scope: KeyScope::$scope }),*
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
    ViewPie => ("Q", "View pie: the views around the pointer (release Q on one, or click it)", "Navigation", Viewport),
    Select => ("W", "Select", "Design / Monitor", Global),
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
    Solo => ("/", "Solo the selection (the numpad's / too)", "Design / Monitor", Global),
    Xray => ("Alt+Z", "X-ray", "Design / Monitor", Global),
    Measure => ("M", "Measure (cycle distance, area, angle, edge, radius, faces, fit)", "Design / Monitor", Global),
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
    FitTurn => ("R", "Turn 90°", "Measure Fit tool", Global),
    FitClear => ("Esc", "Clear fit", "Measure Fit tool", Global),
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

/// The chord `id` answers to now: the person's rebinding, else the table's.
pub fn chord(id: ShortcutId) -> String {
    overrides().get(&id).cloned().unwrap_or_else(|| entry(id).chord.to_string())
}

/// Each alternative is a complete chord; the primary modifier is always `Cmd/Ctrl`.
pub fn shortcut(id: ShortcutId) -> String {
    chord(id).split(" / ").map(makepad_widgets_core::kbd::shortcut_label).collect::<Vec<_>>().join(" / ")
}

// ---- rebinding ---------------------------------------------------------------------------

/// A keyboard chord: a key and its modifiers (`primary` the platform's Cmd/Ctrl, `ctrl` Control
/// where that is another key: on a Mac).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chord {
    pub key: KeyCode,
    pub primary: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// The keys a chord may end in, by the names the table writes them with.
const KEY_NAMES: &[(KeyCode, &str)] = &[
    (KeyCode::KeyA, "A"), (KeyCode::KeyB, "B"), (KeyCode::KeyC, "C"), (KeyCode::KeyD, "D"), (KeyCode::KeyE, "E"),
    (KeyCode::KeyF, "F"), (KeyCode::KeyG, "G"), (KeyCode::KeyH, "H"), (KeyCode::KeyI, "I"), (KeyCode::KeyJ, "J"),
    (KeyCode::KeyK, "K"), (KeyCode::KeyL, "L"), (KeyCode::KeyM, "M"), (KeyCode::KeyN, "N"), (KeyCode::KeyO, "O"),
    (KeyCode::KeyP, "P"), (KeyCode::KeyQ, "Q"), (KeyCode::KeyR, "R"), (KeyCode::KeyS, "S"), (KeyCode::KeyT, "T"),
    (KeyCode::KeyU, "U"), (KeyCode::KeyV, "V"), (KeyCode::KeyW, "W"), (KeyCode::KeyX, "X"), (KeyCode::KeyY, "Y"),
    (KeyCode::KeyZ, "Z"), (KeyCode::Key0, "0"), (KeyCode::Key1, "1"), (KeyCode::Key2, "2"), (KeyCode::Key3, "3"),
    (KeyCode::Key4, "4"), (KeyCode::Key5, "5"), (KeyCode::Key6, "6"), (KeyCode::Key7, "7"), (KeyCode::Key8, "8"),
    (KeyCode::Key9, "9"), (KeyCode::F1, "F1"), (KeyCode::F2, "F2"), (KeyCode::F3, "F3"), (KeyCode::F4, "F4"),
    (KeyCode::F5, "F5"), (KeyCode::F6, "F6"), (KeyCode::F7, "F7"), (KeyCode::F8, "F8"), (KeyCode::F9, "F9"),
    (KeyCode::F10, "F10"), (KeyCode::F11, "F11"), (KeyCode::F12, "F12"), (KeyCode::Delete, "Delete"),
    (KeyCode::Backspace, "Backspace"), (KeyCode::Home, "Home"), (KeyCode::End, "End"), (KeyCode::PageUp, "PageUp"),
    (KeyCode::PageDown, "PageDown"), (KeyCode::Space, "Space"), (KeyCode::Escape, "Esc"), (KeyCode::ReturnKey, "Enter"),
    (KeyCode::Tab, "Tab"), (KeyCode::ArrowUp, "Up"), (KeyCode::ArrowDown, "Down"), (KeyCode::ArrowLeft, "Left"),
    (KeyCode::ArrowRight, "Right"), (KeyCode::Period, "."), (KeyCode::Comma, ","), (KeyCode::Slash, "/"),
    (KeyCode::LBracket, "["), (KeyCode::RBracket, "]"), (KeyCode::Backtick, "`"), (KeyCode::Minus, "-"),
    (KeyCode::Equals, "="), (KeyCode::Semicolon, ";"), (KeyCode::Quote, "'"), (KeyCode::Backslash, "\\"),
    (KeyCode::Numpad0, "Numpad 0"), (KeyCode::Numpad1, "Numpad 1"), (KeyCode::Numpad2, "Numpad 2"),
    (KeyCode::Numpad3, "Numpad 3"), (KeyCode::Numpad4, "Numpad 4"), (KeyCode::Numpad5, "Numpad 5"),
    (KeyCode::Numpad6, "Numpad 6"), (KeyCode::Numpad7, "Numpad 7"), (KeyCode::Numpad8, "Numpad 8"),
    (KeyCode::Numpad9, "Numpad 9"), (KeyCode::NumpadDivide, "Numpad /"),
];

/// One chord as the table writes it ("Cmd/Ctrl+Shift+D"); none for a mouse gesture, a modifier
/// alone or a range ("Cmd/Ctrl+1 … 9"): those are not rebound.
pub fn parse_chord(text: &str) -> Option<Chord> {
    let mut chord = Chord { key: KeyCode::Unknown, primary: false, ctrl: false, shift: false, alt: false };
    let mut rest = text.trim();
    loop {
        if let Some(r) = rest.strip_prefix("Cmd/Ctrl+") { chord.primary = true; rest = r; }
        else if let Some(r) = rest.strip_prefix("Ctrl+") { chord.ctrl = true; rest = r; }
        else if let Some(r) = rest.strip_prefix("Shift+") { chord.shift = true; rest = r; }
        else if let Some(r) = rest.strip_prefix("Alt+") { chord.alt = true; rest = r; }
        else { break; }
    }
    chord.key = KEY_NAMES.iter().find(|(_, name)| *name == rest)?.0;
    Some(chord)
}

/// A chord as the table writes it.
pub fn chord_text(c: Chord) -> String {
    let mut out = String::new();
    if c.primary { out.push_str("Cmd/Ctrl+"); }
    if c.ctrl { out.push_str("Ctrl+"); }
    if c.shift { out.push_str("Shift+"); }
    if c.alt { out.push_str("Alt+"); }
    out.push_str(KEY_NAMES.iter().find(|(k, _)| *k == c.key).map_or("?", |(_, n)| n));
    out
}

/// The chord a key press is. On a Mac Control is its own modifier; elsewhere it is the primary.
pub fn chord_of(ke: &KeyEvent) -> Chord {
    let m = &ke.modifiers;
    Chord { key: ke.key_code, primary: m.is_primary(), ctrl: m.control && !m.is_primary(), shift: m.shift, alt: m.alt }
}

/// A key press as the chord `c` (the same press, other keys held).
fn as_press(ke: &KeyEvent, c: Chord) -> KeyEvent {
    let mut out = ke.clone();
    out.key_code = c.key;
    let primary_is_logo = cfg!(target_vendor = "apple");
    out.modifiers.shift = c.shift;
    out.modifiers.alt = c.alt;
    out.modifiers.logo = c.primary && primary_is_logo;
    out.modifiers.control = c.ctrl || (c.primary && !primary_is_logo);
    out
}

/// Whether `id` can be rebound: every alternative of its chord is a keyboard chord.
pub fn rebindable(id: ShortcutId) -> bool {
    entry(id).chord.split(" / ").all(|c| parse_chord(c).is_some())
}

/// The person's rebindings, loaded from the view prefs the first time they are asked for.
fn overrides() -> std::sync::MutexGuard<'static, HashMap<ShortcutId, String>> {
    static OVERRIDES: std::sync::OnceLock<std::sync::Mutex<HashMap<ShortcutId, String>>> = std::sync::OnceLock::new();
    OVERRIDES
        .get_or_init(|| {
            let saved = crate::view_prefs::load_keymap();
            let map = saved.into_iter()
                .filter_map(|(name, chord)| KEYMAP.iter().find(|e| e.name == name).map(|e| (e.id, chord)))
                .filter(|(id, chord)| rebindable(*id) && parse_chord(chord).is_some())
                .collect();
            std::sync::Mutex::new(map)
        })
        .lock()
        .unwrap()
}

/// Whether `id` is rebound.
pub fn rebound(id: ShortcutId) -> bool {
    overrides().contains_key(&id)
}

/// Rebind `id` to `chord` (`None`, or its own chord: back to the table's), and keep it.
pub fn rebind(id: ShortcutId, chord: Option<Chord>) {
    let text = chord.map(chord_text);
    {
        let mut map = overrides();
        match text {
            Some(t) if t != entry(id).chord => { map.insert(id, t); }
            _ => { map.remove(&id); }
        }
    }
    let pairs: Vec<(String, String)> = KEYMAP.iter().filter_map(|e| overrides().get(&e.id).map(|c| (e.name.to_string(), c.clone()))).collect();
    crate::view_prefs::save_keymap(&pairs);
}

/// Whether the Keymap reference is listening for a new chord: then no press is a shortcut.
static CAPTURING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn capturing() -> bool {
    CAPTURING.load(std::sync::atomic::Ordering::Relaxed)
}

/// What a key press means with the rebindings: itself, the chord of the action it was rebound
/// for (the handlers know the table's chords), or nothing (the table's chord of an action
/// rebound away from it).
pub enum Remap {
    Pass,
    As(KeyEvent),
    Swallow,
}

pub fn remap(ke: &KeyEvent) -> Remap {
    let map = overrides();
    if map.is_empty() { return Remap::Pass; }
    let pressed = chord_of(ke);
    // rebound to this chord: the action's own (first) chord, as the handlers know it
    if let Some((id, _)) = map.iter().find(|(_, c)| parse_chord(c) == Some(pressed)) {
        if let Some(own) = entry(*id).chord.split(" / ").next().and_then(parse_chord) {
            return if own == pressed { Remap::Pass } else { Remap::As(as_press(ke, own)) };
        }
    }
    // the table's chord of an action rebound away from it answers no more
    if map.keys().any(|id| entry(*id).chord.split(" / ").filter_map(parse_chord).any(|c| c == pressed)) {
        return Remap::Swallow;
    }
    Remap::Pass
}

pub fn tooltip(id: ShortcutId) -> String {
    format!("{} ({})", entry(id).action, shortcut(id))
}

/// The table as section headers interleaved with entries (what the sections are built from, for
/// the tests).
#[cfg(test)]
enum HelpRow {
    Section(String),
    Entry(ShortcutId),
}

#[cfg(test)]
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

    // The keymap's sections: one per context (Navigation, Design / Monitor, App …), each a row
    // per action, its chord on a key cap; a rebindable one's cap listens for a new chord when
    // pressed (`FabKeymapSections`).
    mod.widgets.FabKeymapSectionsBase = #(FabKeymapSections::register_widget(vm))
    mod.widgets.FabKeymapSections = set_type_default() do mod.widgets.FabKeymapSectionsBase{
        width: Fill
        height: Fit
        flow: Down
        padding: Inset{left: theme.prop_pad right: theme.prop_pad}
        Section := PropSection{
            grip: false
            body: View{
                width: Fill height: Fit flow: Down
                padding: Inset{top: 4 bottom: 6}
                content := View{width: Fill height: Fit flow: Down spacing: 2}
            }
        }
        Row := View{
            width: Fill
            height: theme.prop_row_height
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            spacing: 8
            padding: Inset{left: 8 right: 4}
            name := Label{
                width: Fill height: Fit
                text: "" max_lines: 1 text_overflow: TextOverflow.Ellipsis
                draw_text +: {
                    color: fab.color_text
                    text_style: theme.font_regular{font_size: theme.prop_font_size line_spacing: 1.0}
                }
            }
            cap := mod.widgets.FabKeyCap{ cap +: { text: "" } }
            reset := mod.widgets.FabLabelMuted{ visible: false text: "↺" }
        }
    }

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
                        mod.widgets.FabLabelMuted{ text: #(format!("{} closes · click a key to change it", shortcut(ShortcutId::Help))) }
                    }
                    // the toolbar: the filter, as every editor's
                    bar := View{
                        width: Fill height: 36 flow: Right align: Align{y: 0.5}
                        search := mod.widgets.FabSearch{width: Fill{min: 80} row +: {input +: {empty_text: "Filter"}}}
                    }
                    body := mod.widgets.FabScroll{
                        height: 520
                        sections := mod.widgets.FabKeymapSections{}
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
            if self.view.text_input(cx, ids!(modal.bar.search.row.input)).changed(actions).is_some() {
                self.view.redraw(cx);
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get_mut::<AppState>() {
            if state.ui.keymap_help_open != self.shown {
                self.shown = state.ui.keymap_help_open;
                let modal = self.view.modal(cx, ids!(modal));
                if self.shown { modal.open(cx); } else { modal.close(cx); }
            }
        }
        let query = self.view.text_input(cx, ids!(modal.bar.search.row.input)).text();
        if let Some(mut sections) = self.view.widget(cx, ids!(modal.body.sections)).borrow_mut::<FabKeymapSections>() {
            sections.query = query;
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

/// The keymap as sections: one per context of the table, in its order, each a row per action.
/// A rebindable action's key cap, pressed, listens for a new chord (Esc keeps the old one); a
/// rebound one shows ↺ to go back to the table's. The toolbar's filter narrows the rows
/// (`widgets::filter_sections`), and the headers fold as every editor's.
#[derive(Script, Widget)]
pub struct FabKeymapSections {
    #[uid]
    uid: WidgetUid,
    #[source]
    source: ScriptObjectRef,
    #[walk]
    walk: Walk,
    #[layout]
    layout: Layout,
    #[rust]
    templates: HashMap<LiveId, ScriptObjectRef>,
    #[rust]
    items: ComponentMap<LiveId, WidgetRef>,
    /// The sections in order, by their context.
    #[rust]
    sections: Vec<(LiveId, &'static str)>,
    /// The filter's words.
    #[rust]
    pub query: String,
    /// The action whose cap listens for a new chord.
    #[rust]
    listening: Option<ShortcutId>,
    #[redraw]
    #[rust]
    area: Area,
}

impl ScriptHook for FabKeymapSections {
    fn on_before_apply(&mut self, _vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, _value: ScriptValue) {
        if apply.is_reload() {
            self.templates.clear();
        }
    }

    fn on_after_apply(&mut self, vm: &mut ScriptVm, apply: &Apply, _scope: &mut Scope, value: ScriptValue) {
        if !apply.is_eval() {
            if let Some(obj) = value.as_object() {
                vm.vec_with(obj, |vm, vec| {
                    for kv in vec {
                        if let (Some(id), Some(t)) = (kv.key.as_id(), kv.value.as_object()) {
                            self.templates.insert(id, vm.new_object_ref(t));
                        }
                    }
                });
            }
        }
        if apply.is_reload() {
            self.items.clear();
            self.sections.clear();
        }
    }
}

/// The filter and section-order key of the keymap's sections.
const KEYMAP_KEY: LiveId = live_id!(keymap_sections);

fn row_id(id: ShortcutId) -> LiveId {
    LiveId::from_str(&format!("key:{}", entry(id).name))
}

impl FabKeymapSections {
    fn item(&mut self, cx: &mut Cx, id: LiveId, template: LiveId) -> Option<WidgetRef> {
        if let Some(w) = self.items.get(&id) {
            return Some(w.clone());
        }
        let value: ScriptValue = self.templates.get(&template)?.as_object().into();
        let w = cx.with_vm(|vm| WidgetRef::script_from_value(vm, value));
        cx.widget_tree_insert_child(self.uid, id, w.clone());
        self.items.insert(id, w.clone());
        Some(w)
    }

    /// The sections and their rows as the table and the rebindings have them now.
    fn build(&mut self, cx: &mut Cx) {
        self.sections.clear();
        let mut contexts: Vec<&'static str> = Vec::new();
        for e in KEYMAP {
            if !contexts.contains(&e.context) { contexts.push(e.context); }
        }
        for context in contexts {
            let sid = LiveId::from_str(&format!("context:{context}"));
            let Some(section) = self.item(cx, sid, live_id!(Section)) else { continue };
            section.label(cx, ids!(hdr.title)).set_text(cx, context);
            let mut rows = Vec::new();
            for e in KEYMAP.iter().filter(|e| e.context == context) {
                let rid = row_id(e.id);
                let Some(row) = self.item(cx, rid, live_id!(Row)) else { continue };
                row.label(cx, ids!(name)).set_text(cx, e.action);
                let cap = if self.listening == Some(e.id) { "Press keys…".to_string() } else { shortcut(e.id) };
                row.label(cx, ids!(cap.cap)).set_text(cx, &cap);
                row.widget(cx, ids!(reset)).set_visible(cx, rebound(e.id));
                rows.push((rid, row));
            }
            crate::ui::widgets::section_rows(cx, &section, rows);
            self.sections.push((sid, context));
        }
    }

    fn sections(&self) -> Vec<WidgetRef> {
        self.sections.iter().filter_map(|(id, _)| self.items.get(id).cloned()).collect()
    }

    fn listen(&mut self, cx: &mut Cx, id: Option<ShortcutId>) {
        self.listening = id;
        CAPTURING.store(id.is_some(), std::sync::atomic::Ordering::Relaxed);
        if id.is_some() {
            cx.set_key_focus(self.area);
        }
        self.area.redraw(cx);
    }
}

impl Widget for FabKeymapSections {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, _scope: &mut Scope) {
        let sections = self.sections();
        crate::ui::widgets::section_headers(cx, event, &sections);
        for s in &sections {
            s.handle_event(cx, event, &mut Scope::empty());
        }
        match event {
            Event::MouseDown(e) if e.button.is_primary() => {
                // a press on a cap listens for its action's new chord; on ↺, back to the table's
                for entry in KEYMAP {
                    let Some(row) = self.items.get(&row_id(entry.id)).cloned() else { continue };
                    if !row.visible() { continue; }
                    let cap = row.widget(cx, ids!(cap)).area();
                    let reset = row.widget(cx, ids!(reset));
                    if reset.visible() && reset.area().is_valid(cx) && reset.area().rect(cx).contains(e.abs) {
                        rebind(entry.id, None);
                        self.listen(cx, None);
                        return;
                    }
                    if cap.is_valid(cx) && cap.rect(cx).contains(e.abs) {
                        if rebindable(entry.id) {
                            self.listen(cx, Some(entry.id));
                        }
                        return;
                    }
                }
                if self.listening.is_some() {
                    self.listen(cx, None);
                }
            }
            Event::KeyDown(ke) if self.listening.is_some() => {
                let id = self.listening.unwrap();
                match ke.key_code {
                    // a modifier alone is part of the chord being pressed
                    KeyCode::Control | KeyCode::Alt | KeyCode::Shift | KeyCode::Logo => {}
                    KeyCode::Escape if !ke.modifiers.shift && !ke.modifiers.alt && !ke.modifiers.control && !ke.modifiers.logo => self.listen(cx, None),
                    _ => {
                        let chord = chord_of(ke);
                        if KEY_NAMES.iter().any(|(k, _)| *k == chord.key) {
                            rebind(id, Some(chord));
                            self.listen(cx, None);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, _scope: &mut Scope, walk: Walk) -> DrawStep {
        self.build(cx);
        let sections = self.sections();
        crate::ui::widgets::filter_sections(cx, KEYMAP_KEY, &self.query, &sections);
        cx.begin_turtle(walk, self.layout);
        for s in &sections {
            if s.visible() {
                s.draw_all(cx, &mut Scope::empty());
            }
        }
        cx.end_turtle_with_area(&mut self.area);
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

    /// A rebinding is kept as the table's own text, so each keyboard chord in the table reads
    /// back as itself, and a mouse gesture or a range is never offered for rebinding.
    #[test]
    fn keyboard_chords_read_back_and_gestures_are_not_rebound() {
        for e in KEYMAP {
            for alt in e.chord.split(" / ") {
                if let Some(c) = parse_chord(alt) {
                    assert_eq!(chord_text(c), alt, "{} reads back", e.name);
                }
            }
        }
        assert!(rebindable(ShortcutId::Undo) && rebindable(ShortcutId::Hide) && rebindable(ShortcutId::Delete));
        assert!(!rebindable(ShortcutId::Orbit) && !rebindable(ShortcutId::Zoom) && !rebindable(ShortcutId::Workspaces));
        assert_eq!(parse_chord("Cmd/Ctrl+Shift+D"), Some(Chord { key: KeyCode::KeyD, primary: true, ctrl: false, shift: true, alt: false }));
    }

    #[test]
    fn every_entry_has_a_section() {
        let mut section = None;
        for row in help_rows() {
            match row {
                HelpRow::Section(title) => section = Some(title),
                HelpRow::Entry(id) => {
                    let e = entry(id);
                    assert!(!e.chord.is_empty() && !e.action.is_empty() && !e.context.is_empty());
                    let title = section.as_deref().unwrap_or_else(|| panic!("{} comes before any section header", e.action));
                    assert!(title.starts_with(e.context), "{} sits under {title:?}, not its own context {}", e.action, e.context);
                }
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
            Tool::Measure(MeasureKind::Radius), Tool::Measure(MeasureKind::FaceDistance), Tool::MeasureFit,
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

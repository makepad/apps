//! The Design workspace's code: tabs for the plate, object scripts and paint files,
//! and the selected object's solids as a tree.
//!
//! [`FabCodePanel`] shows the script's Splash source (the file itself)
//! in Makepad's code editor, highlighted as Splash. Typing is live: every change goes out as
//! [`SourceAction::Edited`], the app evaluates the newest text on the model
//! worker without committing (the view follows), and commits once typing
//! rests, as one undo step. A text that does not build is never committed:
//! the plate stays at the last good model and the problem shows at the end of
//! its line. Values are edited where they are written
//! (`makepad_code_values`): alt-drag a number to scrub it, a swatch before a
//! colour opens a wheel, a chip after a name (a plane, a nozzle, on/off)
//! opens its choices; each gesture is one undo step. Resting the
//! mouse on a value lights up its parameter's handle in the view. The picks' selectors can be
//! typed at the caret ([`InsertCode`]).
//!
//! [`FabFeatureTree`] lists the program's solids in source order: a click
//! selects one (its lines light up in the code and its part in the view); the
//! names edit in place, eyes bypass operands, and siblings can be dragged or moved with
//! the bar's arrows. A solid whose written selection took another count or kind of edge or face
//! than at the build before (the selector report) is ringed, with what changed, to be checked.

use crate::api::*;
use crate::print::trays::DrawFabChip;
use makepad_code_editor::code_view::CodeView;
use makepad_code_editor::text::Position;
use makepad_code_values::{CodeValues, ValueKind, ValueSite};
use makepad_widgets_core::*;
use makepad_widgets_core::tab_bar::{TabBar, TabBarAction};
use std::sync::Arc;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabCodePanelBase = #(FabCodePanel::register_widget(vm))
    mod.widgets.FabCodePanel = set_type_default() do mod.widgets.FabCodePanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        draw_mark: mod.draw.DrawFabChip{}
        draw_mark_text: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        color_error: fab.color_error
        tabs: mod.widgets.TabBarFlat{
            height: 26
            PermanentTab := mod.widgets.TabFlat{
                closeable: false
                height: 26
                padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
                draw_text +: {
                    text_style: theme.font_regular{font_size: fab.font_size_small}
                    color: fab.color_text_dim
                    color_active: fab.color_text_active
                }
            }
        }
        // the file tabs start past the area's nub in the corner (38 wide) and its fillet (8)
        tab_strip := View{width: Fill height: 26 margin: Inset{left: 46}}
        head := View{
            width: Fill
            height: 30
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 10 right: 10 top: 0 bottom: 0}
            spacing: 8
            show_bg: true
            draw_bg +: { color: fab.color_header }
            title := mod.widgets.FabLabel{ text: "Script" draw_text +: { text_style: theme.font_bold{ font_size: fab.font_size_ui } } }
            state := mod.widgets.FabLabelSmall{ text: "" }
            View{ width: Fill height: 1 }
            mod.widgets.FabLabelSmall{ text: #(format!("{} a number to scrub it", crate::ui::keymap::shortcut(crate::ui::keymap::ShortcutId::CodeScrub))) }
        }
        code := CodeView{
            editor +: {
                width: Fill height: Fill
                read_only: false show_gutter: true word_wrap: false
                draw_bg +: { color: fab.color_editor }
                draw_text +: { text_style: theme.font_code{ font_size: 9.0 } }
            }
        }
    }

    mod.widgets.FabFeatureTreeBase = #(FabFeatureTree::register_widget(vm))
    mod.widgets.FabFeatureTree = set_type_default() do mod.widgets.FabFeatureTreeBase{
        width: Fill
        height: Fit
        draw_row: mod.draw.DrawFabChip{ radius: 3.0 }
        draw_folded: mod.draw.DrawSvg{
            svg: crate_resource("self://resources/icons/chevron_right.svg")
            color: fab.color_text_dim
        }
        draw_open: mod.draw.DrawSvg{
            svg: crate_resource("self://resources/icons/chevron_down.svg")
            color: fab.color_text_dim
        }
        draw_eye: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/eye.svg") color: fab.color_text_dim }
        draw_eye_off: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/eye_off.svg") color: fab.color_text_dim }
        draw_link: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/link.svg") color: fab.color_text_dim }
        draw_settings: mod.draw.DrawSvg{ svg: crate_resource("self://resources/icons/settings_here.svg") color: fab.color_text_dim }
        rename_input: mod.widgets.FabInput{ height: 22 a11y: {label: "Feature name"} }
        draw_op: mod.draw.DrawText{
            color: fab.color_text_dim
            text_style: theme.font_code{ font_size: fab.font_size_small }
        }
        draw_name: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_ui }
        }
        color_selected: fab.color_selection_bg
        color_hover: fab.color_row_hover
        color_error: fab.color_error
        color_cut: fab.color_error
        color_check: fab.color_warning
        color_readonly: fab.color_text_dim
    }

    mod.widgets.FabFeaturesPanelBase = #(FabFeaturesPanel::register_widget(vm))
    mod.widgets.FabFeaturesPanel = set_type_default() do mod.widgets.FabFeaturesPanelBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        // A Rust View's quad draws nothing by itself, so the panel fills its own pixel.
        draw_bg +: {
            color: instance(fab.color_panel)
            pixel: fn() {
                return Pal.premul(self.color)
            }
        }
        bar := View{
            width: Fill
            height: 30
            flow: Right
            align: Align{x: 0.0 y: 0.5}
            padding: Inset{left: 8 right: 6 top: 0 bottom: 0}
            spacing: 2
            FabTip{ text: "Add a feature: a primitive, a sketch, a modifier on the picked feature, a pattern, a gear, text"
                add := mod.widgets.FabDropdownButton{
                    a11y: {label: "Add feature"}
                    tag: @fab_add_feature
                    owner: @fab_add_feature
                    label +: { text: "Add" }
                }
            }
            FabTip{ text: "Cut or join a primitive on the picked node, cut the bottom flat or along a plane"
                shape := mod.widgets.FabDropdownButton{
                    a11y: {label: "Shape"}
                    tag: @fab_shape_edit
                    owner: @fab_shape_edit
                    label +: { text: "Shape" }
                }
            }
            count := mod.widgets.FabLabelSmall{ text: "" margin: Inset{left: 6} }
            View{ width: Fill height: 1 }
            FabTip{ text: "Move the feature up (it is built earlier)"
                up := mod.widgets.FabIconButton{ a11y: {label: "Move feature up"} draw_icon +: { svg: crate_resource("self://resources/icons/chevron_up.svg") } }
            }
            FabTip{ text: "Move the feature down (it is built later)"
                down := mod.widgets.FabIconButton{ a11y: {label: "Move feature down"} draw_icon +: { svg: crate_resource("self://resources/icons/chevron_down.svg") } }
            }
            FabTip{ text: "Delete the feature"
                remove := mod.widgets.FabIconButton{ a11y: {label: "Delete feature"} draw_icon +: { svg: crate_resource("self://resources/icons/trash.svg") } }
            }
        }
        mod.widgets.FabScroll{
            width: Fill
            height: Fill
            flow: Down
            tree := mod.widgets.FabFeatureTree{}
        }
    }
}

/// A tree row's fixture or load of the force study (`m.fixed(..)`, `m.force(..)`): no solid.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StudyRow {
    /// Its index in the study of the row's object.
    pub index: usize,
    /// A statement of its own at the script's top (removed from the tree; one in a loop or a
    /// function is edited in the code).
    pub top: bool,
    /// Where, as written (`holes[1]`): what the row shows.
    pub on: String,
}

/// What the tree says of a solid `m.no_trees` marks ([`FeatureRow::detail`]).
pub const NO_TREES: &str = "No trees here";
/// What the tree says of a solid `m.settings` sets print settings in.
pub const SETTINGS_HERE: &str = "Print settings here";
/// What a test print's row says: it is a feature of its model, with no code of its own.
pub const TEST_PRINT: &str = "Test print";
/// A test print's row's kind.
pub const TEST_PRINT_KIND: &str = "test print";
/// What a test print's row's id starts with: a feature of the script may have the same name.
const TEST_PRINT_ID: &str = "test print:";
/// The kind the tree gives a solid `m.settings` sets print settings in, whatever its shape.
pub const SETTINGS_KIND: &str = "print settings";

/// One node of the selected object's component tree: a solid of the committed program, a
/// loop's solids (its instances listed under it), a function call's (collapsed), or a
/// fixture or load of the part's force study.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeatureRow {
    pub id: String,
    pub op: String,
    /// The builder verb or group kind shown and spoken for this row.
    pub verb: String,
    /// The object it makes or changes ("" if none).
    pub object: String,
    /// The solid's host key, empty when it has none.
    pub link: String,
    /// Its bytes and first line in [`SourceState::text`].
    pub range: std::ops::Range<usize>,
    pub line: usize,
    /// Nesting under the tree's top rows (an operand is one deeper than its boolean).
    pub depth: usize,
    /// How it takes part in its parent: `-` cut away, `+` joined, `&` intersected, "" the base.
    pub role: &'static str,
    /// A short description (the import's mesh, the count of a loop's instances).
    pub detail: String,
    /// The rows right under it (they hide while it is collapsed).
    pub children: usize,
    /// Edited through its own code: a loop's instances and a function's insides are not (their
    /// edits go to the loop or the function).
    pub editable: bool,
    /// A loop's or a function call's node, not a solid of its own.
    pub group: bool,
    /// The containing tree row; none for a top-level statement.
    pub parent: Option<String>,
    /// Bypassed in the program.
    pub off: bool,
    /// Emitted roots cannot be toggled here.
    pub root: bool,
    /// A fixture or load, not a solid.
    pub study: Option<StudyRow>,
}

impl FeatureRow {
    /// A test print's row: a feature of its model in the tree, with no code of its own, so no eye.
    pub fn test_print(name: &str, object: &str) -> FeatureRow {
        FeatureRow { id: format!("{TEST_PRINT_ID}{name}"), op: TEST_PRINT_KIND.to_string(), verb: TEST_PRINT_KIND.to_string(), object: object.to_string(), detail: TEST_PRINT.to_string(), ..Default::default() }
    }

    /// What the tree calls it.
    pub fn name(&self) -> &str {
        self.id.strip_prefix(TEST_PRINT_ID).unwrap_or(&self.id)
    }
}

/// One file in the editor. Drafts and diagnostics stay with their tab when focus changes.
#[derive(Clone, Debug, Default)]
pub struct SourceTab {
    pub file: String,
    pub name: String,
    pub text: Arc<String>,
    pub draft: Option<Arc<String>>,
    pub problem: Option<(Option<usize>, String)>,
    pub features: usize,
    pub sites: Vec<(std::ops::Range<usize>, String)>,
}

impl SourceTab {
    pub fn shown_text(&self) -> &str {
        self.draft.as_ref().unwrap_or(&self.text).as_str()
    }
}

/// The program as the code panel and the feature tree see it.
#[derive(Clone, Debug, Default)]
pub struct SourceState {
    /// The selected object's script; the feature tree always follows this file.
    pub file: String,
    /// The file focused in the editor; tab clicks never change the selected object.
    pub viewed: Option<String>,
    pub tabs: Vec<SourceTab>,
    /// The instance of it whose solids the tree's rows pick (`instance/solid`).
    pub instance: String,
    /// The committed text of the selected object's script (for the feature tree).
    pub text: Arc<String>,
    /// Bumped when the panel must show its tab's text again (an edit from elsewhere: the AI, a
    /// handle, undo); the panel's own commits do not bump it, so typing is never reset.
    pub rev: u64,
    /// The committed program's features.
    pub features: Vec<FeatureRow>,
    /// The selected script's problem, also marked in the feature tree.
    pub problem: Option<(Option<usize>, String)>,
    /// The panel's text differs from the committed program (a preview shows, or none builds).
    pub editing: bool,
    /// The feature picked in the tree (its line lights up).
    pub selected: Option<String>,
    /// Tree nodes whose rows are folded away (function calls start folded).
    pub collapsed: std::collections::BTreeSet<String>,
    /// group rows already shown once (a function call starts folded only the first time)
    pub seen: std::collections::BTreeSet<String>,
    /// Scroll the code to a line: (serial, 0-based line).
    pub reveal: (u64, usize),
    /// The committed program's value sites: their bytes and parameter keys (a param's name,
    /// `solid.field[.i]`).
    pub sites: Vec<(std::ops::Range<usize>, String)>,
    /// The selector report of the last build: solids (`instance/solid`) whose written selection
    /// took another count or kind of edge or face than the build before, and what changed. Their
    /// rows are marked to be checked.
    pub drifted: Vec<(String, String)>,
    /// A draft's statements that lost their selector (file, solid, what it lost): each built as
    /// its input until a pick names its edge or face again. Their rows say so.
    pub lost: Vec<(String, String, String)>,
}

impl SourceState {
    pub fn tab(&self) -> Option<&SourceTab> {
        self.tabs.iter().find(|t| Some(&t.file) == self.viewed.as_ref())
    }

    pub fn tab_mut(&mut self) -> Option<&mut SourceTab> {
        self.tabs.iter_mut().find(|t| Some(&t.file) == self.viewed.as_ref())
    }

    pub fn feature(&self, id: &str) -> Option<&FeatureRow> {
        self.features.iter().find(|f| f.id == id)
    }
}

#[derive(Clone, Debug, Default)]
pub enum SourceAction {
    #[default]
    None,
    /// Focus a file without changing the viewport's selection.
    View(String),
    /// The panel's text changed: `gesture` for a value control's edit (its id and whether
    /// it is the gesture's last), `None` for typing.
    Edited { file: String, text: String, gesture: Option<(u64, bool)> },
    /// The value under the resting mouse: its parameter key (a site).
    Hover(Option<String>),
    /// Select a row; only tree-originated selections reveal its code.
    Select { id: String, reveal: bool },
    /// Go to the source a derived row's host key names
    FollowLink(String),
    Fold(String),
    /// The feature row under the pointer (its tool bodies show as ghosts in the view).
    HoverFeature(Option<String>),
    Move { id: String, up: bool },
    /// Move to a sibling's slot.
    Reorder { id: String, to: String },
    /// Rename the row's binding and its uses.
    Rename { id: String, to: String },
    /// Omit or restore a boolean operand.
    Bypass { id: String, off: bool },
    Remove(String),
}

/// Code for the panel showing `file` (a pick's selector): typed at the caret, or over `at`, as
/// one edit.
#[derive(Clone, Debug)]
pub struct InsertCode {
    pub file: String,
    pub text: String,
    /// The bytes it takes the place of, in the text the panel shows (a lost selector named
    /// again); none: the caret's selection.
    pub at: Option<std::ops::Range<usize>>,
}

/// The byte offset of `pos` in `text`.
fn offset_of(text: &str, line: usize, byte: usize) -> usize {
    let mut at = 0;
    for (i, l) in text.split('\n').enumerate() {
        if i == line {
            return (at + byte.min(l.len())).min(text.len());
        }
        at += l.len() + 1;
    }
    text.len()
}

fn position_of(text: &str, at: usize) -> Position {
    let before = &text[..at.min(text.len())];
    Position { line_index: before.matches('\n').count(), byte_index: at - before.rfind('\n').map_or(0, |nl| nl + 1) }
}

/// The parameter key of the value at byte `at`: the site written there.
pub fn key_at(sites: &[(std::ops::Range<usize>, String)], at: usize) -> Option<String> {
    sites.iter().find(|(r, _)| r.start <= at && at <= r.end).map(|(_, k)| k.clone())
}

/// The values of the program that carry a control beyond the number scrub, read from its
/// Splash syntax tree: colours, the names a field takes, switches.
fn sites(text: &str) -> Vec<ValueSite> {
    use makepad_script::syntax::{self, Kind, Node};
    let Ok(tree) = syntax::parse(text) else { return Vec::new() };
    // Typed sites at their spans (file 0): the shared controls pick a chip menu for a choice
    // (written like the value now, `@xy`) or a switch, a swatch for a colour.
    let site = |span: &std::ops::Range<usize>, kind: ValueKind, choices: &[&str]| ValueSite {
        kind,
        choices: choices.iter().map(|c| c.to_string()).collect(),
        span: Some((0, span.clone())),
        ..Default::default()
    };
    let choice = |span: &std::ops::Range<usize>, names: &[&str]| site(span, ValueKind::Choice, names);
    let bools = |span: &std::ops::Range<usize>| site(span, ValueKind::Bool, &[]);
    let mut out = Vec::new();
    // The choices of a field: from the vocabulary when the object's type has that field.
    let field_choices = |ty: Option<&str>, field: &str| -> Option<&'static [&'static str]> {
        let p = makepad_model_script::lang::protos::proto(ty?)?;
        match p.field(field)?.kind {
            makepad_model_script::lang::protos::Kind::Choice(names) => Some(names),
            _ => None,
        }
    };
    let object = |out: &mut Vec<ValueSite>, ty: Option<&str>, fields: &[syntax::Field]| {
        for f in fields {
            let Some(name) = f.name.as_deref() else { continue };
            match &f.value.kind {
                Kind::Name(_) => {
                    if let Some(names) = field_choices(ty, name) {
                        out.push(choice(&f.value.span, names));
                    }
                }
                Kind::Ident(i) if i == "true" || i == "false" => out.push(bools(&f.value.span)),
                _ => {}
            }
        }
    };
    tree.walk(&mut |_, node: &Node| {
        if let Kind::Object { ty: Some(ty), fields, .. } = &node.kind {
            object(&mut out, Some(ty), fields);
        }
        if let Kind::Call { callee, args, .. } = &node.kind {
            // A builder's options object (`m.extrude({plane: @xy ..})`) takes its part's fields.
            let part = callee.ident().and_then(|c| makepad_model_script::lang::protos::of_call(c.rsplit('.').next().unwrap_or(c)));
            for a in args {
                if let Kind::Object { ty: None, fields, .. } = &a.kind {
                    object(&mut out, part.map(|p| p.name), fields);
                }
            }
            for a in args {
                if let Kind::Entry { name, value, .. } = &a.kind {
                    match (&value.kind, name.as_str()) {
                        (Kind::Name(_), "nozzle") => out.push(choice(&value.span, &["any", "left", "right"])),
                        (Kind::Ident(i), _) if i == "true" || i == "false" => out.push(bools(&value.span)),
                        _ => {}
                    }
                }
            }
        }
        if let Kind::Color(hex) = &node.kind {
            if parse_hex(&format!("#{}", hex.trim_start_matches('x'))).is_some() {
                out.push(site(&node.span, ValueKind::Colour, &[]));
            }
        }
    });
    out.sort_by_key(|v| v.range().start);
    out
}

fn parse_hex(s: &str) -> Option<[f32; 4]> {
    let h = s.strip_prefix('#')?;
    let byte = |k: usize| u8::from_str_radix(h.get(k..k + 2)?, 16).ok().map(|v| v as f32 / 255.0);
    match h.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 1.0]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

// ===========================================================================
// The code panel
// ===========================================================================

#[derive(Script, ScriptHook, Widget)]
pub struct FabCodePanel {
    #[deref]
    view: View,
    #[live]
    tabs: TabBar,
    #[live]
    draw_mark: DrawFabChip,
    #[live]
    draw_mark_text: DrawText,
    #[live]
    color_error: Vec4f,
    /// Inline value controls in the code.
    #[live]
    values: CodeValues,
    /// The text the panel holds (the document's, kept in step).
    #[rust]
    text: String,
    #[rust]
    file: String,
    #[rust]
    loaded: Option<u64>,
    #[rust]
    revealed: u64,
    #[rust]
    hover_at: Option<DVec2>,
    #[rust]
    hover_key: Option<String>,
    #[rust]
    hover_timer: Timer,
}

impl FabCodePanel {
    /// The active cursor, read only for navigation events.
    fn cursor(&self, cx: &mut Cx) -> Option<Position> {
        let code = self.view.widget(cx, ids!(code));
        let view = code.borrow::<CodeView>()?;
        let session = view.session.as_ref()?;
        let position = session.selections().get(session.last_added_selection_index().unwrap_or(0))?.cursor.position;
        Some(position)
    }

    fn emit(&mut self, cx: &mut Cx, action: SourceAction) {
        cx.action(action);
    }

    /// The byte under `abs` in the code.
    fn offset_at(&self, cx: &mut Cx, abs: DVec2) -> Option<usize> {
        let code = self.view.widget(cx, ids!(code));
        let view = code.borrow::<CodeView>()?;
        let session = view.session.as_ref()?;
        let origin = view.editor.area().rect(cx).pos;
        let hit = view.editor.hit_test(session, abs - origin)?;
        Some(offset_of(&self.text, hit.position.line_index, hit.position.byte_index))
    }

    fn document_version(&self, cx: &mut Cx) -> Option<u64> {
        let code = self.view.widget(cx, ids!(code));
        let view = code.borrow::<CodeView>()?;
        Some(view.session.as_ref()?.document().version())
    }

    /// The document's text as it is now.
    fn document_text(&self, cx: &mut Cx) -> Option<String> {
        let code = self.view.widget(cx, ids!(code));
        let view = code.borrow::<CodeView>()?;
        let text = view.session.as_ref().map(|s| s.document().as_text().to_string());
        text
    }
}

impl FabCodePanel {
    /// `text` at the caret (over the selection), or over bytes `at` of the document, then out as
    /// an edit like typing.
    fn insert_at_caret(&mut self, cx: &mut Cx, text: &str, at: Option<&std::ops::Range<usize>>) {
        use makepad_code_editor::{history::NewGroup, selection::Affinity, session::SelectionMode};
        let code = self.view.widget(cx, ids!(code));
        if let Some(mut view) = code.borrow_mut::<CodeView>() {
            if let Some(session) = view.session.as_mut() {
                if let Some(at) = at.filter(|at| at.end <= self.text.len() && self.text.is_char_boundary(at.start) && self.text.is_char_boundary(at.end)) {
                    session.set_selection(position_of(&self.text, at.start), Affinity::After, SelectionMode::Simple, NewGroup::Yes);
                    session.move_to(position_of(&self.text, at.end), Affinity::Before, NewGroup::No);
                }
                session.insert(text.into());
            }
            view.redraw(cx);
        }
        let Some(now) = self.document_text(cx).filter(|now| *now != self.text) else { return };
        self.text = now;
        self.values.set_sites(&self.text, &sites(&self.text));
        let text = self.text.clone();
        self.emit(cx, SourceAction::Edited { file: self.file.clone(), text, gesture: None });
    }

    fn has_key_focus(&self, cx: &mut Cx) -> bool {
        let code = self.view.widget(cx, ids!(code));
        let focused = code.borrow::<CodeView>().is_some_and(|view| !view.editor.area().is_empty() && cx.has_key_focus(view.editor.area()));
        focused
    }
}

impl Widget for FabCodePanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let actions = cx.capture_actions(|cx| self.tabs.handle_event(cx, event, scope));
        let later = match event { Event::Actions(actions) => Some(actions), _ => None };
        for action in actions.iter().chain(later.into_iter().flatten()) {
            let Some(action) = action.as_widget_action().filter(|a| a.widget_uid == self.tabs.widget_uid()) else { continue };
            if let TabBarAction::TabWasPressed(id) = Some(action).cast() {
                let file = scope.data.get::<AppState>().and_then(|s| s.source.tabs.iter().find(|t| LiveId::from_str(&t.file) == id)).map(|t| t.file.clone());
                if let Some(file) = file {
                    self.emit(cx, SourceAction::View(file));
                }
            }
        }
        cx.extend_actions(actions);
        // Code the app asks for at the caret (a pick's selector), in the file it is for.
        for insert in later.into_iter().flatten().filter_map(|a| a.downcast_ref::<InsertCode>()) {
            if insert.file == self.file {
                self.insert_at_caret(cx, &insert.text, insert.at.as_ref());
            }
        }
        // A value's control (a scrub, a swatch, a chip) first: the shared inline controls.
        {
            let code = self.view.widget(cx, ids!(code));
            let r = match code.borrow::<CodeView>() {
                Some(view) => self.values.handle_event(cx, event, scope, Some(&view)),
                None => Default::default(),
            };
            for edit in r.edits {
                let (gesture, done) = (edit.gesture, edit.done);
                // Its byte changes of the program (file 0), from the end so none moves another.
                let Ok(splices) = makepad_edit_values::normalize_splices(edit.splices().filter(|s| s.file == 0).cloned().collect()) else { continue };
                for sp in splices {
                let (start, end, text) = (sp.range.start, sp.range.end, sp.text);
                if start > end || end > self.text.len() || !self.text.is_char_boundary(start) || !self.text.is_char_boundary(end) {
                    continue;
                }
                if let Some(mut view) = code.borrow_mut::<CodeView>() {
                    if let Some(session) = view.session.as_mut() {
                        // As given: typing's insert would autoindent the splice.
                        session.document().replace_range(position_of(&self.text, start), position_of(&self.text, end), text.as_str().into());
                        session.handle_changes();
                    }
                    view.redraw(cx);
                }
                self.text.replace_range(start..end, &text);
                }
                self.values.set_sites(&self.text, &sites(&self.text));
                let text = self.text.clone();
                self.emit(cx, SourceAction::Edited { file: self.file.clone(), text, gesture: Some((gesture, done)) });
            }
            if r.redraw {
                self.view.redraw(cx);
            }
            if r.consumed {
                return;
            }
        }
        // The value under the resting mouse lights up its handle.
        if self.hover_timer.is_event(event).is_some() {
            if let Some(abs) = self.hover_at {
                let key = scope.data.get::<AppState>().and_then(|st| st.source.tab()).filter(|tab| tab.text.as_str() == self.text).and_then(|tab| self.offset_at(cx, abs).and_then(|at| key_at(&tab.sites, at)));
                if key != self.hover_key {
                    self.hover_key = key.clone();
                    self.emit(cx, SourceAction::Hover(key));
                }
            }
        }
        // Cmd/Ctrl+Z with nothing typed since the last commit is the document's undo (one
        // history for every edit); while typing is still uncommitted the editor undoes it.
        if let Event::KeyDown(ke) = event {
            if ke.key_code == KeyCode::KeyZ && ke.modifiers.is_primary() && self.has_key_focus(cx) {
                let pending = scope.data.get::<AppState>().is_some_and(|s| s.source.editing);
                if !pending {
                    cx.action(ShellAction::Command(if ke.modifiers.shift { "redo" } else { "undo" }.into()));
                    return;
                }
            }
        }
        let edit_event = matches!(event, Event::KeyDown(_) | Event::TextInput(_) | Event::TextCut(_) | Event::TextRangeReplace(_) | Event::ImeAction(_));
        let pointer_event = matches!(event, Event::MouseDown(_) | Event::MouseUp(_) | Event::MouseMove(_));
        let focused = (edit_event || pointer_event) && self.has_key_focus(cx);
        let navigation = pointer_event
            || (focused && matches!(event, Event::KeyDown(k) if matches!(k.key_code, KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::ArrowUp | KeyCode::ArrowDown | KeyCode::Home | KeyCode::End | KeyCode::PageUp | KeyCode::PageDown)));
        let before = navigation.then(|| self.cursor(cx)).flatten();
        let version = (focused && edit_event).then(|| self.document_version(cx)).flatten();
        self.view.handle_event(cx, event, scope);
        let changed = version.is_some_and(|before| self.document_version(cx).is_some_and(|after| before != after));
        let editor_area = if pointer_event {
            let code = self.view.widget(cx, ids!(code));
            let area = code.borrow::<CodeView>().map_or(Area::Empty, |view| view.editor.area());
            area
        } else { Area::Empty };
        let clicked_code = matches!(event, Event::MouseDown(_)) && !editor_area.is_empty() && event.pointer_claimed_area() == editor_area;
        if let Event::MouseMove(e) = event {
            let inside = !editor_area.is_empty() && event.pointer_claimed_area() == editor_area;
            if inside && self.hover_at.is_none_or(|p| (p - e.abs).length() > 3.0) {
                self.hover_at = Some(e.abs);
                cx.stop_timer(self.hover_timer);
                self.hover_timer = cx.start_timeout(0.25);
            } else if !inside {
                self.hover_at = None;
                cx.stop_timer(self.hover_timer);
                if self.hover_key.take().is_some() { self.emit(cx, SourceAction::Hover(None)); }
            }
        }
        if navigation && !changed && self.has_key_focus(cx) {
            if let Some(pos) = self.cursor(cx).filter(|p| Some(*p) != before || clicked_code) {
                let at = offset_of(&self.text, pos.line_index, pos.byte_index);
                let row = scope.data.get::<AppState>().map(|s| &s.source)
                    .filter(|s| s.file == self.file && !s.editing && self.loaded == Some(s.rev))
                    .and_then(|s| s.features.iter().filter(|f| f.range.contains(&at))
                        .min_by_key(|f| (f.range.len(), std::cmp::Reverse(f.depth)))
                        .filter(|f| s.selected.as_ref() != Some(&f.id))).map(|f| f.id.clone());
                if let Some(id) = row {
                    self.emit(cx, SourceAction::Select { id, reveal: false });
                }
            }
        }
        // Typing, a paste, a cut, the editor's own undo: the text changed.
        if changed {
            if let Some(now) = self.document_text(cx) {
                if now != self.text {
                    self.text = now;
                    self.values.set_sites(&self.text, &sites(&self.text));
                    let text = self.text.clone();
                    self.emit(cx, SourceAction::Edited { file: self.file.clone(), text, gesture: None });
                    self.view.redraw(cx);
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let (reload, reveal, problem, wash, word) = match scope.data.get::<AppState>() {
            Some(state) => {
                let s = &state.source;
                let tab = s.tab();
                let reload = (self.loaded != Some(s.rev)).then(|| (s.rev, tab.map_or("", SourceTab::shown_text).to_string()));
                let selected_file = s.viewed.as_ref() == Some(&s.file);
                let reveal = (selected_file && s.reveal.0 != self.revealed).then_some(s.reveal);
                // The picked feature's lines, in the panel's text when it is the program's.
                let wash = s.selected.as_ref().filter(|_| selected_file).and_then(|id| s.feature(id)).map(|f| {
                    let lines = s.text[f.range.clone()].matches('\n').count();
                    f.line..f.line + lines + 1
                });
                let problem = tab.and_then(|t| t.problem.clone());
                let count = tab.map_or(0, |t| t.features);
                let word = match (&problem, s.editing) {
                    (Some((Some(l), _)), _) => format!("Not built: line {}", l + 1),
                    (Some((None, _)), _) => "Not built".to_string(),
                    (None, true) => "Building…".to_string(),
                    (None, false) => format!("{count} feature{}", if count == 1 { "" } else { "s" }),
                };
                (reload, reveal, problem, wash, word)
            }
            None => (None, None, None, None, String::new()),
        };
        self.view.label(cx, ids!(head.state)).set_text(cx, &word);
        let title = scope.data.get::<AppState>().and_then(|st| st.source.viewed.clone()).unwrap_or_else(|| "Program".to_string());
        self.view.label(cx, ids!(head.title)).set_text(cx, &title);
        let code = self.view.widget(cx, ids!(code));
        if let Some((rev, text)) = reload {
            if let Some(mut view) = code.borrow_mut::<CodeView>() {
                view.set_text_for_path(cx, &title, &text);
            }
            self.text = text;
            if self.file != title {
                // A popup/gesture belongs to its old document, even if another file has a
                // colour at the same byte offset.
                self.values = cx.with_vm(|vm| CodeValues::script_new(vm));
            }
            self.file = title;
            self.values.set_sites(&self.text, &sites(&self.text));
            self.loaded = Some(rev);
            self.hover_key = None;
            self.hover_at = None;
            cx.stop_timer(self.hover_timer);
        }
        if let Some(mut view) = code.borrow_mut::<CodeView>() {
            if let Some((serial, line)) = reveal {
                self.revealed = serial;
                view.editor.center_line(cx, line);
            }
            view.editor.line_washes = wash.map(|r| vec![(r, 0.5)]).unwrap_or_default();
            view.lazy_init_session();
            self.values.sync(&mut view);
        }
        while self.view.draw_walk(cx, scope, walk).is_step() {}
        if let Some(state) = scope.data.get::<AppState>() {
            let active = state.source.tabs.iter().position(|t| Some(&t.file) == state.source.viewed.as_ref());
            let rect = self.view.widget(cx, ids!(tab_strip)).area().rect(cx);
            self.tabs.begin(cx, active, true, Walk::abs_rect(rect));
            for tab in &state.source.tabs {
                self.tabs.draw_tab(cx, LiveId::from_str(&tab.file), &tab.name, live_id!(PermanentTab));
            }
            self.tabs.end(cx);
        }
        let Some(view) = code.borrow::<CodeView>() else { return DrawStep::done() };
        let Some(session) = view.session.as_ref() else { return DrawStep::done() };
        let area = view.editor.area().rect(cx);
        let metrics = view.editor.metrics(session);
        let clip = Rect { pos: dvec2(area.pos.x + metrics.gutter_width, area.pos.y), size: dvec2((area.size.x - metrics.gutter_width - 10.0).max(0.0), area.size.y) };
        // The problem at the end of its line (or the first line).
        let chip = problem.and_then(|(line, message)| {
            let line = line.unwrap_or(0);
            let lines = session.document().as_text().as_lines().iter().map(|l| l.len()).collect::<Vec<_>>();
            let end = *lines.get(line)?;
            let cell = view.editor.position_rect(session, Position { line_index: line, byte_index: end })?;
            Some((area.pos + cell.pos + dvec2(16.0, 0.0), cell.size.y, message))
        });
        drop(view);
        if let Some((at, h, message)) = chip {
            cx.begin_turtle(Walk::abs_rect(clip), Layout::default().with_clip(true, true));
            let w = self.draw_mark_text.prepare_single_line_run(cx, &message).map(|r| r.width_in_lpxs as f64).unwrap_or(100.0) + 14.0;
            let rect = Rect { pos: dvec2(at.x, at.y + 1.0), size: dvec2(w, (h - 2.0).max(15.0)) };
            let c = self.color_error;
            self.draw_mark.fill = vec4(c.x, c.y, c.z, 0.22);
            self.draw_mark.ring = vec4(c.x, c.y, c.z, 0.9);
            self.draw_mark.radius = 3.0;
            self.draw_mark.draw_abs(cx, rect);
            self.draw_mark_text.draw_abs(cx, dvec2(rect.pos.x + 7.0, rect.pos.y + (rect.size.y - 11.0) * 0.5), &message);
            cx.end_turtle();
        }
        if let Some(view) = code.borrow::<CodeView>() {
            self.values.draw(cx, scope, &view, clip);
        }
        DrawStep::done()
    }
}

// ===========================================================================
// The feature tree
// ===========================================================================

/// The tree under its bar (move up, move down, delete the picked feature).
#[derive(Script, ScriptHook, Widget)]
pub struct FabFeaturesPanel {
    #[deref]
    view: View,
}

impl Widget for FabFeaturesPanel {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
        // A click reaches the window's actions once, a pointer's (its button's action passed
        // on) as a semantic press's (`ui act`): taken there alone, a button acts once. (Taken
        // where the button emits it too, one click moved twice or removed twice.)
        let Event::Actions(actions) = event else { return };
        let Some(id) = scope.data.get::<AppState>().and_then(|s| s.source.selected.clone()) else { return };
        if self.view.button(cx, ids!(bar.up)).clicked(actions) {
            cx.action(SourceAction::Move { id: id.clone(), up: true });
        }
        if self.view.button(cx, ids!(bar.down)).clicked(actions) {
            cx.action(SourceAction::Move { id: id.clone(), up: false });
        }
        if self.view.button(cx, ids!(bar.remove)).clicked(actions) {
            cx.action(SourceAction::Remove(id));
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            let n = state.source.features.len();
            let on = state.source.selected.is_some();
            self.view.label(cx, ids!(bar.count)).set_text(cx, &format!("{n} nodes"));
            for id in [ids!(bar.up), ids!(bar.down), ids!(bar.remove)] {
                self.view.button(cx, id).set_enabled(cx, on);
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

const ROW: f64 = 24.0;

/// Hit regions and source identity of a visible row.
#[derive(Clone)]
struct FeatureHit {
    rect: Rect,
    name: Rect,
    fold: Option<Rect>,
    eye: Option<Rect>,
    link: Option<Rect>,
    key: String,
    label: String,
    id: String,
    parent: Option<String>,
    editable: bool,
    bypass: bool,
    off: bool,
}

/// A row held by the pointer and its current sibling destination.
struct FeatureDrag {
    id: String,
    parent: Option<String>,
    start: DVec2,
    at: DVec2,
    moved: bool,
    to: Option<(String, bool)>,
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabFeatureTree {
    #[uid]
    uid: WidgetUid,
    #[walk]
    walk: Walk,
    #[redraw]
    #[live]
    draw_row: DrawFabChip,
    #[live]
    draw_folded: DrawSvg,
    #[live]
    draw_open: DrawSvg,
    #[live]
    draw_eye: DrawSvg,
    #[live]
    draw_eye_off: DrawSvg,
    #[live]
    draw_link: DrawSvg,
    /// A print settings box's kind: it has no eye (a box is no shape to hide).
    #[live]
    draw_settings: DrawSvg,
    #[live]
    rename_input: WidgetRef,
    #[live]
    draw_op: DrawText,
    #[live]
    draw_name: DrawText,
    #[live]
    color_selected: Vec4f,
    #[live]
    color_hover: Vec4f,
    #[live]
    color_error: Vec4f,
    #[live]
    color_cut: Vec4f,
    /// A row the selector report asks to check.
    #[live]
    color_check: Vec4f,
    #[live]
    color_readonly: Vec4f,
    #[rust]
    rows: Vec<FeatureHit>,
    #[rust]
    rename: Option<String>,
    #[rust]
    rename_focus: bool,
    #[rust]
    drag: Option<FeatureDrag>,
    #[rust]
    drag_list: Option<DrawList2d>,
    #[rust]
    hover: Option<usize>,
    #[rust]
    selected: Option<String>,
    #[rust]
    area: Area,
}

impl FabFeatureTree {
    /// Edit the binding in place; only Enter sends an edit.
    fn begin_rename(&mut self, cx: &mut Cx, id: String) {
        if !self.rows.iter().any(|r| r.id == id && r.editable) {
            return;
        }
        self.rename_input.set_text(cx, &id);
        self.rename = Some(id);
        self.rename_focus = true;
        self.drag = None;
        self.area.redraw(cx);
    }

    /// The overlay must redraw also on release, to erase the pointer's chip.
    fn redraw_drag(&mut self, cx: &mut Cx) {
        self.area.redraw(cx);
        if let Some(list) = &self.drag_list {
            list.redraw(cx);
        }
    }
}

impl Widget for FabFeatureTree {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::List;
        makepad_platform::a11y::set_string(&mut node.name, "Features");
        node.set_size = self.rows.len() as u32;
    }

    fn semantics_parts(&self, _cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        for (n, hit) in self.rows.iter().enumerate() {
            let mut node = a11y_tree::Node {
                role: a11y_tree::Role::ListItem,
                bounds: makepad_platform::a11y::semantic_rect(hit.rect),
                pos_in_set: n as u32 + 1,
                set_size: self.rows.len() as u32,
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::SELECT,
                ..Default::default()
            };
            if self.selected.as_ref() == Some(&hit.id) {
                node.states.set(a11y_tree::States::SELECTED, true);
            }
            makepad_platform::a11y::set_string(&mut node.name, &hit.label);
            out.push((n as u64 + 1, None, node));
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _scope: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if request.action != Actions::PRESS && request.action != Actions::SELECT {
            return ActionResult::Unhandled;
        }
        let Some(key) = cx.a11y.action_part_key(request.node) else { return ActionResult::Unhandled };
        let Some(hit) = self.rows.get((key as usize).wrapping_sub(1)) else { return ActionResult::Unsupported };
        cx.set_key_focus(self.area);
        cx.action(SourceAction::Select { id: hit.id.clone(), reveal: true });
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if self.rename.is_some() {
            let actions = cx.capture_actions(|cx| self.rename_input.handle_event(cx, event, scope));
            let input = self.rename_input.as_text_input();
            let later = match event { Event::Actions(a) => Some(a), _ => None };
            let escaped = input.escaped(&actions) || later.is_some_and(|a| input.escaped(a));
            let returned = input.returned(&actions).or_else(|| later.and_then(|a| input.returned(a)));
            if escaped || returned.is_some() {
                if let (Some(id), Some((to, _))) = (self.rename.take(), returned) {
                    if id != to { cx.action(SourceAction::Rename { id, to }); }
                }
                cx.set_key_focus(self.area);
                self.area.redraw(cx);
            }
            cx.extend_actions(actions);
        }
        if let Event::KeyDown(k) = event {
            if k.key_code == KeyCode::F2 && !k.is_repeat && cx.keyboard.key_focus() == self.area {
                if let Some(id) = self.selected.clone() { self.begin_rename(cx, id); }
                return;
            }
            if k.key_code == KeyCode::Escape && self.drag.take().is_some() {
                self.redraw_drag(cx);
            }
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(fe) | Hit::FingerHoverOver(fe) => {
                let h = self.rows.iter().position(|r| r.rect.contains(fe.abs));
                if h.is_some_and(|i| self.rows[i].editable || self.rows[i].link.is_some_and(|r| r.contains(fe.abs))) { cx.set_cursor(MouseCursor::Hand); }
                if h != self.hover {
                    self.hover = h;
                    self.area.redraw(cx);
                    cx.action(SourceAction::HoverFeature(h.and_then(|i| self.rows.get(i)).map(|r| r.id.clone())));
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.hover.take().is_some() {
                    self.area.redraw(cx);
                    cx.action(SourceAction::HoverFeature(None));
                }
            }
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                cx.set_key_focus(self.area);
                if let Some(hit) = self.rows.iter().find(|r| r.rect.contains(fe.abs)).cloned() {
                    if !hit.fold.is_some_and(|r| r.contains(fe.abs)) && !hit.eye.is_some_and(|r| r.contains(fe.abs)) && !hit.link.is_some_and(|r| r.contains(fe.abs)) && hit.editable {
                        self.drag = Some(FeatureDrag { id: hit.id, parent: hit.parent, start: fe.abs, at: fe.abs, moved: false, to: None });
                        self.redraw_drag(cx);
                    }
                }
            }
            Hit::FingerMove(fe) => {
                if let Some(drag) = &mut self.drag {
                    drag.at = fe.abs;
                    drag.moved |= (drag.at - drag.start).length() > 4.0;
                    let from = self.rows.iter().position(|r| r.id == drag.id);
                    drag.to = self.rows.iter().enumerate().find(|(_, r)| r.rect.contains(fe.abs) && r.editable && r.id != drag.id && r.parent == drag.parent)
                        .map(|(i, r)| (r.id.clone(), from.is_some_and(|from| from < i)));
                    cx.set_cursor(MouseCursor::Hand);
                    self.redraw_drag(cx);
                }
            }
            Hit::FingerUp(fe) => {
                let drag = self.drag.take();
                self.redraw_drag(cx);
                if fe.cancelled || !fe.is_over || !fe.device.is_primary_hit() { return; }
                if let Some(drag) = drag.filter(|d| d.moved) {
                    if let Some(hit) = self.rows.iter().find(|r| r.rect.contains(fe.abs) && r.editable && r.id != drag.id && r.parent == drag.parent) {
                        cx.action(SourceAction::Reorder { id: drag.id, to: hit.id.clone() });
                    }
                    return;
                }
                if let Some(hit) = self.rows.iter().find(|r| r.rect.contains(fe.abs)).cloned() {
                    if hit.link.is_some_and(|r| r.contains(fe.abs)) {
                        cx.action(SourceAction::FollowLink(hit.key));
                    } else if hit.fold.is_some_and(|r| r.contains(fe.abs)) {
                        cx.action(SourceAction::Fold(hit.id));
                    } else if hit.eye.is_some_and(|r| r.contains(fe.abs)) {
                        if hit.bypass {
                            cx.action(SourceAction::Bypass { id: hit.id, off: !hit.off });
                        }
                    } else {
                        cx.action(SourceAction::Select { id: hit.id.clone(), reveal: true });
                        if fe.tap_count == 2 && hit.name.contains(fe.abs) { self.begin_rename(cx, hit.id); }
                    }
                }
            }
            _ => {}
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.rows.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let s = &state.source;
        self.selected = s.selected.clone();
        if self.rename.as_ref().is_some_and(|id| s.selected.as_ref() != Some(id) || s.feature(id).is_none()) {
            self.rename = None;
        }
        let mut hidden = None;
        let visible: Vec<_> = s.features.iter().filter(|f| {
            if hidden.is_some_and(|depth| f.depth > depth) {
                return false;
            }
            hidden = s.collapsed.contains(&f.id).then_some(f.depth);
            true
        }).collect();
        let mut walk = walk;
        walk.height = Size::Fixed(visible.len() as f64 * ROW + 8.0);
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        let failing = s.problem.as_ref().and_then(|(l, _)| *l);
        let name_color = self.draw_name.color;
        let dim_color = self.draw_op.color;
        let mut rename_rect = None;
        for (n, f) in visible.into_iter().enumerate() {
            let r = Rect { pos: dvec2(rect.pos.x + 4.0, rect.pos.y + 4.0 + n as f64 * ROW), size: dvec2(rect.size.x - 8.0, ROW - 2.0) };
            let on = s.selected.as_ref() == Some(&f.id);
            let bad = failing == Some(f.line);
            // a draft's statement that lost its selector, or the selector report: a selection of
            // this solid took something else than before
            let lost = s.lost.iter().find(|(file, id, _)| *file == s.file && *id == f.id).map(|(_, _, what)| format!("{what}: pick it"));
            let drift = s.drifted.iter().find(|(part, _)| part.strip_prefix(s.instance.as_str()).and_then(|p| p.strip_prefix('/')) == Some(f.id.as_str())).map(|(_, what)| format!("check: {what}"));
            let (mark, mark_color) = match (lost, drift) {
                (Some(l), _) => (Some(l), self.color_error),
                (None, d) => (d, self.color_check),
            };
            let fill = if on {
                self.color_selected
            } else if self.hover == Some(n) {
                self.color_hover
            } else {
                vec4(0.0, 0.0, 0.0, 0.0)
            };
            self.draw_row.fill = fill;
            self.draw_row.ring = if bad { self.color_error } else if mark.is_some() { mark_color } else { vec4(0.0, 0.0, 0.0, 0.0) };
            self.draw_row.spool = 0.0;
            self.draw_row.draw_abs(cx, r);
            let settings = f.detail == SETTINGS_HERE;
            let eye = (f.editable && !settings).then_some(Rect { pos: dvec2(r.pos.x + r.size.x - 22.0, r.pos.y), size: dvec2(22.0, r.size.y) });
            if let Some(eye) = eye {
                let icon = Rect { pos: eye.pos + dvec2(3.0, 4.0), size: dvec2(14.0, 14.0) };
                let mut color = dim_color;
                if f.root || f.op == "call" { color.w *= 0.35; }
                let draw = if f.off { &mut self.draw_eye_off } else { &mut self.draw_eye };
                draw.color = color;
                draw.draw_abs(cx, icon);
            }
            let link = (!f.link.is_empty()).then_some(Rect { pos: dvec2(eye.map_or(r.pos.x + r.size.x, |eye| eye.pos.x) - 22.0, r.pos.y), size: dvec2(22.0, r.size.y) });
            if let Some(link) = link {
                self.draw_link.color = dim_color;
                self.draw_link.draw_abs(cx, Rect { pos: link.pos + dvec2(3.0, 4.0), size: dvec2(14.0, 14.0) });
            }
            let text_end = link.map_or(r.pos.x + r.size.x - 24.0, |link| link.pos.x - 2.0);
            let x = r.pos.x + 6.0 + f.depth as f64 * 10.0;
            let fold = (f.children > 0).then_some(Rect { pos: dvec2(x, r.pos.y), size: dvec2(14.0, r.size.y) });
            if let Some(fold) = fold {
                let icon = Rect { pos: fold.pos + dvec2(0.0, 4.0), size: dvec2(14.0, 14.0) };
                if s.collapsed.contains(&f.id) {
                    self.draw_folded.draw_abs(cx, icon);
                } else {
                    self.draw_open.draw_abs(cx, icon);
                }
            }
            let (glyph, role) = match f.role {
                "-" => ("−", "cut"),
                "+" => ("+", "joined"),
                "&" => ("∩", "intersected"),
                _ => ("", "base"),
            };
            self.draw_op.color = if f.role == "-" { self.color_cut } else { dim_color };
            if f.off { self.draw_op.color.w *= 0.4; }
            if settings {
                self.draw_settings.draw_abs(cx, Rect { pos: dvec2(x + 11.0, r.pos.y + 4.0), size: dvec2(14.0, 14.0) });
            } else {
                self.draw_op.draw_abs(cx, dvec2(x + 14.0, r.pos.y + 6.0), glyph);
            }
            self.draw_op.color = dim_color;
            if f.off { self.draw_op.color.w *= 0.4; }
            // the kind column is as wide as the kind's own text, capped at 74 and 0.4 of the row
            let kind_w = self.draw_op.prepare_single_line_run(cx, &f.verb).map_or(74.0, |run| run.width_in_lpxs as f64 + 1.0);
            let op_w = kind_w.min(74.0).min((text_end - x - 26.0).max(0.0) * 0.4);
            cx.begin_turtle(Walk::abs_rect(Rect { pos: dvec2(x + 26.0, r.pos.y), size: dvec2(op_w, r.size.y) }), Layout::default().with_clip(true, true));
            self.draw_op.draw_abs(cx, dvec2(x + 26.0, r.pos.y + 6.0), &f.verb);
            cx.end_turtle();
            let name = f.study.as_ref().map_or_else(|| f.name().to_string(), |s| s.on.clone());
            // a gap after the clipped kind, so a long one never runs into the name
            let at = dvec2(x + 26.0 + op_w + 6.0, r.pos.y + 5.0);
            let name_rect = Rect { pos: dvec2(at.x, r.pos.y), size: dvec2((text_end - at.x).max(0.0), r.size.y) };
            self.draw_name.color = if f.editable || f.detail == TEST_PRINT { name_color } else { self.color_readonly };
            if f.off { self.draw_name.color.w *= 0.4; }
            if self.rename.as_ref() == Some(&f.id) {
                rename_rect = Some(name_rect);
            } else {
                cx.begin_turtle(Walk::abs_rect(name_rect), Layout::default().with_clip(true, true));
                self.draw_name.draw_abs(cx, at, &name);
            }
            // a box's kind says what it is; a lost or drifted selection what it needs
            let detail = mark.as_deref().unwrap_or(&f.detail);
            if self.rename.as_ref() != Some(&f.id) && !detail.is_empty() && !settings {
                if let Some(run) = self.draw_name.prepare_single_line_run(cx, &name) {
                    self.draw_name.color = if mark.is_some() { mark_color } else { dim_color };
                    if f.off { self.draw_name.color.w *= 0.4; }
                    self.draw_name.draw_abs(cx, at + dvec2(run.width_in_lpxs as f64 + 8.0, 0.0), detail);
                }
            }
            if self.rename.as_ref() != Some(&f.id) { cx.end_turtle(); }
            if let Some((_, after)) = self.drag.as_ref().filter(|d| d.moved).and_then(|d| d.to.as_ref()).filter(|(to, _)| *to == f.id) {
                self.draw_row.fill = name_color;
                self.draw_row.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_row.draw_abs(cx, Rect { pos: dvec2(x, if *after { r.pos.y + ROW - 2.0 } else { r.pos.y - 2.0 }), size: dvec2((r.pos.x + r.size.x - x).max(0.0), 2.0) });
            }
            self.rows.push(FeatureHit { rect: r, name: name_rect, fold, eye, link, key: f.link.clone(), label: format!("{role} {} {} level {}{}{}{}", f.verb, f.name(), f.depth + 1, if f.off { " bypassed" } else { "" }, if f.link.is_empty() { "" } else { " linked" }, mark.as_ref().map(|what| format!(", {what}")).unwrap_or_default()), id: f.id.clone(), parent: f.parent.clone(), editable: f.editable, bypass: f.editable && !f.root && f.op != "call" && !settings, off: f.off });
        }
        self.draw_name.color = name_color;
        self.draw_op.color = dim_color;
        if let Some(rect) = rename_rect {
            while self.rename_input.draw_walk(cx, &mut Scope::empty(), Walk::abs_rect(rect)).is_step() {}
            if self.rename_focus {
                let input = self.rename_input.as_text_input();
                input.set_key_focus(cx);
                if let Some(mut input) = input.borrow_mut() { input.select_all(cx); }
                self.rename_focus = false;
            }
        }
        if self.drag.is_some() || self.drag_list.is_some() {
            let list = self.drag_list.get_or_insert_with(|| DrawList2d::new(cx));
            list.begin_overlay_reuse(cx);
            let pass = cx.current_pass_size();
            cx.begin_root_turtle(pass, Layout::flow_down());
            let shift = if let Some(drag) = &self.drag {
                let w = self.draw_name.prepare_single_line_run(cx, &drag.id).map_or(0.0, |r| r.width_in_lpxs as f64) + 16.0;
                self.draw_row.fill = self.color_selected;
                self.draw_row.ring = name_color;
                self.draw_row.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(w, ROW) });
                self.draw_name.draw_abs(cx, dvec2(8.0, 5.0), &drag.id);
                drag.at + dvec2(12.0, 12.0) - rect.pos
            } else { DVec2::default() };
            cx.end_pass_sized_turtle_with_shift(self.area, shift);
            list.end(cx);
        }
        DrawStep::done()
    }
}

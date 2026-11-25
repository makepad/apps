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
use crate::ui::popover::{menu_picked, open_menu, ui_actions, FabUiAction, MenuItem, MenuPlace};
use makepad_widgets_core::tip::{TipAction, TipRequest};
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
            text_style: theme.font_regular{ font_size: fab.font_size_small }
        }
        draw_name: mod.draw.DrawText{
            color: fab.color_text
            text_style: theme.font_regular{ font_size: fab.font_size_ui }
        }
        color_selected: fab.color_selection_bg
        // the name's ink under the pointer (the row's ground stays; only the name lights)
        color_hover: fab.color_text_hover
        color_error: fab.color_error
        // the role marks after the names: a solid disc, its sign in `color_mark_ink`
        color_cut: fab.color_warning
        color_join: fab.color_accent
        color_mark_ink: fab.color_editor
        color_check: fab.color_warning
        color_readonly: fab.color_text_dim
        // the kind before a name ("cylinder"): quieter than the name and the row's icons
        color_kind: fab.color_text_muted
        // the dragged row's chip, as the app's tooltips look
        draw_tip: mod.draw.FabTipChip{}
        draw_tip_text: mod.draw.FabTipText{}
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
    /// Its role in its boolean can be changed (join, cut, intersect): an editable operand, not
    /// the base, of a plain `m.union` / `m.subtract` / `m.intersect`.
    pub can_role: bool,
    /// A fixture or load, not a solid.
    pub study: Option<StudyRow>,
}

impl FeatureRow {
    /// A test print's row: a feature of its model in the tree, with no code of its own, so no eye.
    pub fn test_print(name: &str, object: &str) -> FeatureRow {
        FeatureRow { id: format!("{TEST_PRINT_ID}{name}"), op: TEST_PRINT_KIND.to_string(), object: object.to_string(), detail: TEST_PRINT.to_string(), ..Default::default() }
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
    /// Every placed instance's tree by instance name, made with each committed program: the
    /// Parts outliner shows a part's tree under it. The selected script's is `features`.
    pub trees: std::collections::HashMap<String, Arc<Vec<FeatureRow>>>,
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
    /// Make this instance the one the tree's edits and the code follow: a part of it is
    /// selected (a press in an outliner tree of another part comes first with this).
    Focus(String),
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
    /// Make a boolean operand joined (`+`), cut away (`-`) or intersected (`&`), keeping the
    /// order the operands apply in.
    SetRole { id: String, role: &'static str },
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
/// A tree level's indent, the Parts outliner's.
const INDENT: f64 = 18.0;
/// How long a fold takes to open or close (s), in the tree and in the Parts outliner (a fn, not
/// a const: a live session patches a fn's new value in, a const's needs a restart).
pub fn fold_s() -> f64 {
    0.1
}

/// A fold's travel 0..1, eased: it starts and ends softly.
pub fn fold_ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// A loop's row: its copies (`holes[0]`..) start folded under it (its kind says what each is, how
/// many there are is in its tooltip); unfolded they show, each to hover (its ghost in the view)
/// or pick. In `SourceState::collapsed` a loop's id means unfolded, the other way round from every
/// other row, so a loop nobody opened stays folded. A pick of a copy (in the view) lights the loop's
/// row while it is folded, and a mark on a copy shows on it then.
fn is_loop(f: &FeatureRow) -> bool {
    f.group && f.op != "call"
}

/// A group row's tooltip: what its children are, in place of a count on the row.
fn group_tip(f: &FeatureRow) -> Option<String> {
    if !f.group { return None; }
    Some(if is_loop(f) {
        format!("{} {}{}, made by a loop", f.children, f.op, if f.children == 1 { "" } else { "s" })
    } else {
        format!("A function call: {}", f.detail)
    })
}

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
    /// The role disc after the name, and whether a press on it opens the role menu.
    mark: Option<Rect>,
    role: &'static str,
    can_role: bool,
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
    #[live]
    color_join: Vec4f,
    #[live]
    color_mark_ink: Vec4f,
    /// A row the selector report asks to check.
    #[live]
    color_check: Vec4f,
    #[live]
    color_readonly: Vec4f,
    #[live]
    color_kind: Vec4f,
    #[live]
    draw_tip: DrawFabChip,
    #[live]
    draw_tip_text: DrawText,
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
    /// The instance whose tree this shows (the Parts outliner sets it per part); none: the
    /// selected script's.
    #[rust]
    pub instance: Option<String>,
    /// It shows the selected script's tree, the one edits apply to; another instance's is
    /// focused (made the selected one) before a press's edit.
    #[rust]
    current: bool,
    /// The row the context menu is up for.
    #[rust]
    menu_row: Option<String>,
    /// Each fold's openness as drawn (0 folded .. 1 open) and the one it heads for; while one
    /// moves the tree draws again every frame and its rows under it close up or open out.
    #[rust]
    folds: std::collections::HashMap<String, (f64, f64)>,
    #[rust]
    fold_frame: NextFrame,
    #[rust]
    fold_last: Option<f64>,
    /// How far the Parts outliner has this tree open (0..1, eased here); none: all of it.
    #[rust]
    reveal: Option<f64>,
    #[rust]
    area: Area,
}

impl FabFeatureTree {
    /// Show `instance`'s tree, its rows starting `indent` in (the Parts outliner, under its part).
    pub fn show_instance(&mut self, instance: &str, indent: f64, reveal: f64) {
        if self.instance.as_deref() != Some(instance) {
            self.instance = Some(instance.to_string());
            self.rename = None;
            self.drag = None;
            self.hover = None;
            self.folds.clear();
        }
        self.walk.margin.left = indent;
        self.reveal = Some(reveal);
    }

    /// A source action of a press in this tree: on another instance's tree, that instance is
    /// focused first, so the action applies to its script.
    fn act(&self, cx: &mut Cx, action: SourceAction) {
        if let (false, Some(instance)) = (self.current, &self.instance) {
            cx.action(SourceAction::Focus(instance.clone()));
        }
        cx.action(action);
    }

    fn menu_id(&self) -> LiveId {
        LiveId::from_str(&format!("fab_feature_menu:{:?}", self.uid))
    }

    fn role_menu_id(&self) -> LiveId {
        LiveId::from_str(&format!("fab_feature_role:{:?}", self.uid))
    }

    /// The rows this tree shows: the selected script's, or its instance's.
    fn feature_rows<'a>(&self, s: &'a SourceState) -> &'a [FeatureRow] {
        if self.current { &s.features } else { self.instance.as_ref().and_then(|i| s.trees.get(i)).map_or(&[], |t| t.as_slice()) }
    }

    /// Why a disc's role cannot change here, said in its menu (every disc opens one).
    fn role_note(&self, s: &SourceState, hit: &FeatureHit) -> Option<String> {
        if hit.can_role { return None; }
        let rows = self.feature_rows(s);
        let parent = hit.parent.as_ref().and_then(|id| rows.iter().find(|f| f.id == *id));
        Some(match parent {
            Some(p) if p.group && p.op != "call" => format!("A copy made by {}: change the role of {} ×{} instead", p.name(), p.name(), p.children),
            _ if !hit.editable => "Made inside a function: change its role where the function is written".into(),
            Some(p) => format!("A {}'s operands keep their roles: change it in the code", p.op),
            None => "Its role cannot change here: change it in the code".into(),
        })
    }

    /// The role disc's menu: join, cut or intersect, the current one checked; where the role
    /// cannot change, the same items unavailable under the reason.
    /// A smooth boolean's operand (a blend's or a carve's): it joins or cuts with the boolean's
    /// radius, and nothing intersects smoothly.
    fn smooth_operand(&self, s: &SourceState, hit: &FeatureHit) -> bool {
        let rows = self.feature_rows(s);
        hit.parent.as_ref().and_then(|id| rows.iter().find(|f| f.id == *id)).is_some_and(|p| p.op == "blend" || p.op == "carve")
    }

    fn open_role_menu(&mut self, cx: &mut Cx, hit: &FeatureHit, under: Rect, note: Option<String>, smooth: bool) {
        self.menu_row = Some(hit.id.clone());
        let can = note.is_none();
        let mut items = Vec::new();
        if let Some(note) = note {
            items.push(MenuItem::new(live_id!(role_note), &note).disabled());
            items.push(MenuItem::sep());
        }
        items.extend([
            MenuItem::new(live_id!(role_join), if smooth { "Join smoothly (+)" } else { "Join (+)" }).checked(hit.role == "+").enabled_if(can),
            MenuItem::new(live_id!(role_cut), if smooth { "Cut away smoothly (−)" } else { "Cut away (−)" }).checked(hit.role == "-").enabled_if(can),
            MenuItem::new(live_id!(role_intersect), if smooth { "Intersect (not with a smooth edge)" } else { "Intersect (∩)" }).checked(hit.role == "&").enabled_if(can && !smooth),
        ]);
        open_menu(cx, self.role_menu_id(), items, under, MenuPlace::Below);
    }

    /// The row's menu: what the Features panel's bar held (move, delete) and rename.
    fn open_row_menu(&mut self, cx: &mut Cx, hit: &FeatureHit, at: DVec2) {
        self.menu_row = Some(hit.id.clone());
        let items = vec![
            MenuItem::new(live_id!(feature_rename), "Rename").enabled_if(hit.editable),
            MenuItem::sep(),
            MenuItem::new(live_id!(feature_up), "Move up (built earlier)").enabled_if(hit.editable),
            MenuItem::new(live_id!(feature_down), "Move down (built later)").enabled_if(hit.editable),
            MenuItem::sep(),
            MenuItem::new(live_id!(feature_remove), "Delete").enabled_if(hit.editable),
        ];
        open_menu(cx, self.menu_id(), items, Rect { pos: at, size: dvec2(0.0, 0.0) }, MenuPlace::At);
    }

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
        let id = hit.id.clone();
        self.act(cx, SourceAction::Select { id, reveal: true });
        ActionResult::Handled
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if let Event::NextFrame(ne) = event {
            if ne.set.contains(&self.fold_frame) {
                let dt = self.fold_last.map_or(0.0, |t| (ne.time - t).clamp(0.0, 0.05));
                self.fold_last = Some(ne.time);
                let step = dt / fold_s();
                let mut moving = false;
                for (now, to) in self.folds.values_mut() {
                    if now != to {
                        *now = if *to > *now { (*now + step).min(*to) } else { (*now - step).max(*to) };
                        moving |= now != to;
                    }
                }
                if moving { self.fold_frame = cx.new_next_frame(); } else { self.fold_last = None; }
                self.area.redraw(cx);
            }
        }
        if self.rename.is_some() {
            let actions = cx.capture_actions(|cx| self.rename_input.handle_event(cx, event, scope));
            let input = self.rename_input.as_text_input();
            let later = match event { Event::Actions(a) => Some(a), _ => None };
            let escaped = input.escaped(&actions) || later.is_some_and(|a| input.escaped(a));
            let returned = input.returned(&actions).or_else(|| later.and_then(|a| input.returned(a)));
            // a press elsewhere takes the focus: the name typed is kept, as Return keeps it
            let lost = !escaped && returned.is_none() && (input.key_focus_lost(&actions) || later.is_some_and(|a| input.key_focus_lost(a)));
            let typed = returned.map(|(to, _)| to).or_else(|| lost.then(|| input.text()));
            if escaped || typed.is_some() {
                if let (Some(id), Some(to)) = (self.rename.take(), typed) {
                    let to = to.trim().to_string();
                    if !to.is_empty() && id != to { self.act(cx, SourceAction::Rename { id, to }); }
                }
                if !lost { cx.set_key_focus(self.area); }
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
                    let tip = h.and_then(|i| self.rows.get(i)).and_then(|hit| {
                        let state = scope.data.get::<AppState>()?;
                        let f = self.feature_rows(&state.source).iter().find(|f| f.id == hit.id)?;
                        Some((group_tip(f)?, hit.rect))
                    });
                    let uid = self.widget_uid();
                    cx.widget_action(uid, match tip {
                        Some((text, rect)) => TipAction::HoverInWith(TipRequest { wrap_width: 320.0, ..TipRequest::plain(text, rect) }),
                        None => TipAction::HoverOut,
                    });
                    if self.current {
                        cx.action(SourceAction::HoverFeature(h.and_then(|i| self.rows.get(i)).map(|r| r.id.clone())));
                    }
                }
            }
            Hit::FingerHoverOut(_) => {
                if self.hover.take().is_some() {
                    self.area.redraw(cx);
                    let uid = self.widget_uid();
                    cx.widget_action(uid, TipAction::HoverOut);
                    if self.current {
                        cx.action(SourceAction::HoverFeature(None));
                    }
                }
            }
            Hit::FingerDown(fe) if fe.device.is_primary_hit() => {
                cx.set_key_focus(self.area);
                if let Some(hit) = self.rows.iter().find(|r| r.rect.contains(fe.abs)).cloned() {
                    // the eye and the role disc act as they are pressed
                    if hit.eye.is_some_and(|r| r.contains(fe.abs)) {
                        if hit.bypass {
                            self.act(cx, SourceAction::Bypass { id: hit.id, off: !hit.off });
                        }
                        return;
                    }
                    // a fold folds as it is pressed
                    if hit.fold.is_some_and(|r| r.contains(fe.abs)) {
                        self.act(cx, SourceAction::Fold(hit.id));
                        return;
                    }
                    if let Some(mark) = hit.mark.filter(|r| r.contains(fe.abs)) {
                        let (note, smooth) = scope.data.get::<AppState>().map_or((None, false), |state| (self.role_note(&state.source, &hit), self.smooth_operand(&state.source, &hit)));
                        self.act(cx, SourceAction::Select { id: hit.id.clone(), reveal: false });
                        self.open_role_menu(cx, &hit, mark, note, smooth);
                        return;
                    }
                    let on_control = hit.fold.is_some_and(|r| r.contains(fe.abs)) || hit.link.is_some_and(|r| r.contains(fe.abs));
                    if !on_control {
                        // a row is picked as it is pressed (a drag of it moves it after)
                        self.act(cx, SourceAction::Select { id: hit.id.clone(), reveal: true });
                        if fe.tap_count == 2 && hit.name.contains(fe.abs) {
                            self.begin_rename(cx, hit.id.clone());
                        } else if hit.editable {
                            self.drag = Some(FeatureDrag { id: hit.id, parent: hit.parent, start: fe.abs, at: fe.abs, moved: false, to: None });
                        }
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
                if !fe.cancelled && fe.is_over && fe.device.mouse_button().is_some_and(|b| b.is_secondary()) {
                    if let Some(hit) = self.rows.iter().find(|r| r.rect.contains(fe.abs)).cloned() {
                        let id = hit.id.clone();
                        self.act(cx, SourceAction::Select { id, reveal: false });
                        self.open_row_menu(cx, &hit, fe.abs);
                    }
                    return;
                }
                if fe.cancelled || !fe.is_over || !fe.device.is_primary_hit() { return; }
                if let Some(drag) = drag.filter(|d| d.moved) {
                    if let Some(hit) = self.rows.iter().find(|r| r.rect.contains(fe.abs) && r.editable && r.id != drag.id && r.parent == drag.parent) {
                        let to = hit.id.clone();
                        self.act(cx, SourceAction::Reorder { id: drag.id, to });
                    }
                    return;
                }
                if let Some(hit) = self.rows.iter().find(|r| r.rect.contains(fe.abs)).cloned() {
                    if hit.link.is_some_and(|r| r.contains(fe.abs)) {
                        cx.action(SourceAction::FollowLink(hit.key));
                    }
                }
            }
            _ => {}
        }
        if let Event::Actions(actions) = event {
            // A press that dismissed an open menu and landed on a role disc opens that disc's menu
            // on the same press, unless it is the disc whose menu it closed (that press was the
            // toggle). The close and its click-away come in the same actions.
            let closed_ours = ui_actions(actions).any(|a| matches!(a, FabUiAction::MenuClosed { owner } if *owner == self.role_menu_id()));
            let away = ui_actions(actions).find_map(|a| match a { FabUiAction::MenuClickAway { at } => Some(*at), _ => None });
            if let Some(at) = away {
                if let Some(hit) = self.rows.iter().find(|r| r.mark.is_some_and(|m| m.contains(at))).cloned() {
                    let toggled = closed_ours && self.menu_row.as_deref() == Some(hit.id.as_str());
                    if !toggled {
                        let (note, smooth) = scope.data.get::<AppState>().map_or((None, false), |state| (self.role_note(&state.source, &hit), self.smooth_operand(&state.source, &hit)));
                        self.act(cx, SourceAction::Select { id: hit.id.clone(), reveal: false });
                        self.open_role_menu(cx, &hit, hit.mark.unwrap_or(hit.rect), note, smooth);
                        return;
                    }
                }
            }
            // (the row is let go only by this menu's own pick)
            if let Some((pick, id)) = menu_picked(actions, self.role_menu_id()).and_then(|pick| Some((pick, self.menu_row.take()?))) {
                let role = if pick == live_id!(role_join) { "+" } else if pick == live_id!(role_cut) { "-" } else { "&" };
                self.act(cx, SourceAction::SetRole { id, role });
            }
            if let Some((pick, id)) = menu_picked(actions, self.menu_id()).and_then(|pick| Some((pick, self.menu_row.take()?))) {
                if pick == live_id!(feature_rename) {
                    self.begin_rename(cx, id);
                } else if pick == live_id!(feature_up) || pick == live_id!(feature_down) {
                    self.act(cx, SourceAction::Move { id, up: pick == live_id!(feature_up) });
                } else if pick == live_id!(feature_remove) {
                    self.act(cx, SourceAction::Remove(id));
                }
            }
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.rows.clear();
        let Some(state) = scope.data.get::<AppState>() else { return DrawStep::done() };
        let s = &state.source;
        // The selected script's tree is the one edits apply to, with its picks, folds and marks;
        // another instance's shows its rows as built (function calls folded), unpicked.
        let current = self.instance.as_ref().is_none_or(|i| *i == s.instance);
        self.current = current;
        let features: &[FeatureRow] = if current {
            &s.features
        } else {
            self.instance.as_ref().and_then(|i| s.trees.get(i)).map_or(&[], |t| t.as_slice())
        };
        let is_folded = |f: &FeatureRow| if current { s.collapsed.contains(&f.id) != is_loop(f) } else { f.group };
        // a copy of a folded loop picked (in the view): its loop's row shows it
        let selected = if current { s.selected.clone() } else { None }.map(|id| {
            let parent = features.iter().find(|f| f.id == id).and_then(|f| f.parent.as_ref()).and_then(|p| features.iter().find(|f| f.id == *p));
            parent.filter(|p| is_loop(p) && is_folded(p)).map_or(id, |p| p.id.clone())
        });
        self.selected = selected.clone();
        if self.rename.as_ref().is_some_and(|id| !features.iter().any(|f| f.id == *id)) {
            self.rename = None;
        }
        // Each row with how open its folds above it are (1: all open; between: a fold moving, the
        // row's height that share of a row's). A loop's copies never show.
        let mut visible: Vec<(&FeatureRow, f64)> = Vec::new();
        let mut stack: Vec<(usize, f64)> = Vec::new();
        let mut moving = false;
        for f in features {
            while stack.last().is_some_and(|(d, _)| *d >= f.depth) { stack.pop(); }
            let above = stack.last().map_or(1.0, |(_, k)| *k);
            if above > 0.0 { visible.push((f, above)); }
            let open = if f.children > 0 {
                let to = if is_folded(f) { 0.0 } else { 1.0 };
                let fold = self.folds.entry(f.id.clone()).or_insert((to, to));
                fold.1 = to;
                moving |= fold.0 != fold.1;
                fold_ease(fold.0)
            } else {
                1.0
            };
            stack.push((f.depth, above * open));
        }
        if moving { self.fold_frame = cx.new_next_frame(); }
        let full: f64 = visible.iter().map(|(_, k)| ROW * k).sum::<f64>() + 8.0;
        let mut walk = walk;
        walk.height = Size::Fixed((full * fold_ease(self.reveal.unwrap_or(1.0))).round());
        let rect = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, rect);
        // (the outliner opening it: what is not open yet is cut off)
        cx.begin_turtle(Walk::abs_rect(rect), Layout::default().with_clip(true, true));
        let mut top = rect.pos.y + 4.0;
        let failing = s.problem.as_ref().and_then(|(l, _)| *l).filter(|_| current);
        let name_color = self.draw_name.color;
        let dim_color = self.draw_op.color;
        let mut rename_rect = None;
        for (n, (f, k)) in visible.into_iter().enumerate() {
            let r = Rect { pos: dvec2(rect.pos.x + 4.0, top), size: dvec2(rect.size.x - 8.0, ROW - 2.0) };
            // a row of a fold that is moving: its share of a row's height, the rest cut off
            let slot = Rect { pos: dvec2(rect.pos.x, top), size: dvec2(rect.size.x, ROW * k) };
            top += ROW * k;
            let partial = k < 1.0;
            if partial { cx.begin_turtle(Walk::abs_rect(slot), Layout::default().with_clip(true, true)); }
            let on = selected.as_ref() == Some(&f.id);
            let bad = failing == Some(f.line);
            // a draft's statement that lost its selector, or the selector report: a selection of
            // this solid took something else than before
            // (a folded loop's row carries its copies' marks, which show nowhere else then)
            let rolled = is_loop(f) && is_folded(f);
            let names = |id: &str| id == f.id || (rolled && features.iter().any(|c| c.id == id && c.parent.as_deref() == Some(f.id.as_str())));
            let lost = s.lost.iter().filter(|_| current).find(|(file, id, _)| *file == s.file && names(id)).map(|(_, _, what)| format!("{what}: pick it"));
            let drift = s.drifted.iter().filter(|_| current).find(|(part, _)| part.strip_prefix(s.instance.as_str()).and_then(|p| p.strip_prefix('/')).is_some_and(|id| names(id))).map(|(_, what)| format!("check: {what}"));
            let (mark, mark_color) = match (lost, drift) {
                (Some(l), _) => (Some(l), self.color_error),
                (None, d) => (d, self.color_check),
            };
            let fill = if on { self.color_selected } else { vec4(0.0, 0.0, 0.0, 0.0) };
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
            // The Parts outliner's grid: a level is 18 wide, a fold's slot is the level's first 18
            // (its 10-wide chevron centred in it) and the kind starts 6 after it, so a part's
            // top rows fold under its swatch and their kinds start where its name does.
            let x = r.pos.x + 6.0 + f.depth as f64 * INDENT;
            let fold = (f.children > 0).then_some(Rect { pos: dvec2(x, r.pos.y), size: dvec2(INDENT, r.size.y) });
            if let Some(fold) = fold {
                let icon = Rect { pos: fold.pos + dvec2((INDENT - 10.0) * 0.5, (fold.size.y - 10.0) * 0.5), size: dvec2(10.0, 10.0) };
                if is_folded(f) {
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
            // after the fold's column: a print settings box's icon, then the kind
            let mut kx = x + INDENT + 6.0;
            if settings {
                self.draw_settings.draw_abs(cx, Rect { pos: dvec2(kx, r.pos.y + 4.0), size: dvec2(14.0, 14.0) });
                kx += 18.0;
            }
            // The name first, then its kind (quieter), its role disc and its detail; all of it cut
            // off where the row's icons start.
            let name = f.study.as_ref().map_or_else(|| f.name().to_string(), |s| s.on.clone());
            let mut mark_rect = None;
            let at = dvec2(kx, r.pos.y + 5.0);
            let name_rect = Rect { pos: dvec2(at.x, r.pos.y), size: dvec2((text_end - at.x).max(0.0), r.size.y) };
            let editable = f.editable || f.detail == TEST_PRINT;
            self.draw_name.color = if !editable { self.color_readonly } else if self.hover == Some(n) { self.color_hover } else { name_color };
            if f.off { self.draw_name.color.w *= 0.4; }
            if self.rename.as_ref() == Some(&f.id) {
                rename_rect = Some(name_rect);
            } else {
                cx.begin_turtle(Walk::abs_rect(name_rect), Layout::default().with_clip(true, true));
                self.draw_name.draw_abs(cx, at, &name);
                let mut after = at.x + self.draw_name.prepare_single_line_run(cx, &name).map_or(0.0, |run| run.width_in_lpxs as f64) + 6.0;
                // its kind ("cylinder"), in the quietest ink
                if !f.op.is_empty() {
                    self.draw_op.color = self.color_kind;
                    if f.off { self.draw_op.color.w *= 0.4; }
                    self.draw_op.draw_abs(cx, dvec2(after, r.pos.y + 6.0), &f.op);
                    self.draw_op.color = dim_color;
                    after += self.draw_op.prepare_single_line_run(cx, &f.op).map_or(0.0, |run| run.width_in_lpxs as f64) + 6.0;
                }
                // How it takes part in its parent, in a disc after the kind: − cut away, + joined,
                // ∩ intersected; the base has none.
                if !glyph.is_empty() {
                    mark_rect = Some(Rect { pos: dvec2(after - 2.0, r.pos.y), size: dvec2(17.0, r.size.y) });
                    // a solid disc: yellow for a cut, blue for a join, grey for an intersection
                    let mut disc = match f.role { "-" => self.color_cut, "+" => self.color_join, _ => dim_color };
                    let mut ink = self.color_mark_ink;
                    if f.off { disc.w *= 0.4; ink.w *= 0.4; }
                    let d = 13.0;
                    let ring = Rect { pos: dvec2(after, r.pos.y + (r.size.y - d) * 0.5), size: dvec2(d, d) };
                    // the ring in the fill's colour: a clear ring's soft edge would eat into a
                    // shape this small (the bars came out faint)
                    self.draw_row.fill = disc;
                    self.draw_row.ring = disc;
                    self.draw_row.radius = (d * 0.5) as f32;
                    self.draw_row.draw_abs(cx, ring);
                    // + and − as bars on the disc's centre (a glyph's ink sits off it)
                    let c = ring.pos + ring.size * 0.5;
                    let (len, thick) = (7.0, 2.0);
                    self.draw_row.fill = ink;
                    self.draw_row.ring = ink;
                    self.draw_row.radius = 0.5;
                    if f.role == "-" || f.role == "+" {
                        self.draw_row.draw_abs(cx, Rect { pos: dvec2(c.x - len * 0.5, c.y - thick * 0.5), size: dvec2(len, thick) });
                    }
                    if f.role == "+" {
                        self.draw_row.draw_abs(cx, Rect { pos: dvec2(c.x - thick * 0.5, c.y - len * 0.5), size: dvec2(thick, len) });
                    }
                    if f.role == "&" {
                        self.draw_op.color = ink;
                        let gw = self.draw_op.prepare_single_line_run(cx, glyph).map_or(6.0, |run| run.width_in_lpxs as f64);
                        self.draw_op.draw_abs(cx, dvec2(c.x - gw * 0.5, r.pos.y + 6.0), glyph);
                        self.draw_op.color = dim_color;
                    }
                    self.draw_row.radius = 3.0;
                    after = ring.pos.x + d + 6.0;
                }
                // a box's kind says what it is; a lost or drifted selection what it needs
                // (a group's count is in its tooltip)
                let detail = mark.as_deref().unwrap_or(if f.group { "" } else { f.detail.as_str() });
                if !detail.is_empty() && !settings {
                    self.draw_name.color = if mark.is_some() { mark_color } else { dim_color };
                    if f.off { self.draw_name.color.w *= 0.4; }
                    self.draw_name.draw_abs(cx, dvec2(after, at.y), detail);
                }
                cx.end_turtle();
            }
            if let Some((_, after)) = self.drag.as_ref().filter(|d| d.moved).and_then(|d| d.to.as_ref()).filter(|(to, _)| *to == f.id) {
                self.draw_row.fill = name_color;
                self.draw_row.ring = vec4(0.0, 0.0, 0.0, 0.0);
                self.draw_row.draw_abs(cx, Rect { pos: dvec2(x, if *after { r.pos.y + ROW - 2.0 } else { r.pos.y - 2.0 }), size: dvec2((r.pos.x + r.size.x - x).max(0.0), 2.0) });
            }
            if partial { cx.end_turtle(); }
            self.rows.push(FeatureHit { rect: r, name: name_rect, fold, eye, link, key: f.link.clone(), label: format!("{role} {} {} level {}{}{}{}", f.op, f.name(), f.depth + 1, if f.off { " bypassed" } else { "" }, if f.link.is_empty() { "" } else { " linked" }, mark.as_ref().map(|what| format!(", {what}")).unwrap_or_default()), id: f.id.clone(), parent: f.parent.clone(), editable: f.editable, bypass: f.editable && !f.root && f.op != "call" && !settings, off: f.off, mark: mark_rect, role: f.role, can_role: f.can_role });
        }
        cx.end_turtle();
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
                // the row's name in a tooltip's bubble (the app's one look for what rides the pointer)
                let w = self.draw_tip_text.prepare_single_line_run(cx, &drag.id).map_or(0.0, |r| r.width_in_lpxs as f64) + 16.0;
                self.draw_tip.draw_abs(cx, Rect { pos: dvec2(0.0, 0.0), size: dvec2(w, 24.0) });
                self.draw_tip_text.draw_abs(cx, dvec2(8.0, 6.0), &drag.id);
                drag.at + dvec2(12.0, 12.0) - rect.pos
            } else { DVec2::default() };
            cx.end_pass_sized_turtle_with_shift(self.area, shift);
            list.end(cx);
        }
        DrawStep::done()
    }
}

//! A pack's preset picker: the widget a pack registers for choosing its
//! presets (tiles with thumbnails, by lane), as a host drives it without
//! knowing the pack. The widget's type name is [`EffectPack::picker_widget`];
//! a host finds the picker behind a widget with [`with_picker`] and reads
//! what the user did from [`PickerAction`]s.
//!
//! [`EffectPack::picker_widget`]: crate::EffectPack::picker_widget

use makepad_widgets::*;

/// What a picker's user did (a widget action of the picker).
#[derive(Clone, Debug, Default)]
pub enum PickerAction {
    #[default]
    None,
    Open,
    Choose { preset: String, title: String, category: String, shift: bool },
    /// A tile dragged out of the grid (a managed picker): the pointer is at `abs`.
    Drag { preset: String, title: String, category: String, abs: Vec2d },
    /// A dragged tile let go at `abs`; `why` says why not when the effect is
    /// unavailable, for the host to refuse it.
    Drop { preset: String, title: String, category: String, abs: Vec2d, why: Option<String> },
}

/// An unmanaged picker asks its host to set an authored graph parameter
/// (`node.key = value_json`) of the instance it shows, through the host's
/// own undoable write. Posted as an action.
#[derive(Clone, Debug)]
pub struct PickerParamWrite {
    pub instance: String,
    /// From a package's app surface rather than a node face.
    pub surface: bool,
    pub node: String,
    pub key: String,
    pub value_json: String,
}

/// One preset as the picker lists it now (its lane, in its order).
#[derive(Clone, Debug)]
pub struct ListedPreset {
    pub preset: String,
    pub title: String,
    /// The tile's sub line.
    pub sub: String,
    pub thumbnail: Option<Texture>,
    /// Why not, when it is unavailable.
    pub why: Option<String>,
}

/// A picker as a host drives it.
pub trait PresetPicker {
    /// Show one lane (`"transition"`, `"text"`, a category, `""` for every effect).
    fn set_category(&mut self, cx: &mut Cx, category: &str);
    /// Load the catalogue without being drawn (a host listing it itself).
    fn ensure_loaded(&mut self, cx: &mut Cx);
    /// The presets shown now.
    fn listed(&self) -> Vec<ListedPreset>;
    /// The parameter whose value marks the running tile.
    fn set_target(&mut self, cx: &mut Cx, target: &str);
}

/// Run `f` on the picker behind `widget`, if an installed pack's picker it
/// is. False when it is none (no pack, or another widget).
pub fn with_picker(widget: &WidgetRef, f: &mut dyn FnMut(&mut dyn PresetPicker)) -> bool {
    crate::effect_packs().iter().any(|pack| pack.with_picker(widget, f))
}

/// The pages' `EffectPicker` when no pack provides one (`mod.widgets`, from
/// [`script_mod`]): it takes a picker's properties and shows nothing, and
/// [`with_picker`] finds no picker behind it.
#[derive(Script, ScriptHook, Widget)]
pub struct NoEffectPicker {
    #[deref]
    view: View,
    #[live]
    pub target: String,
    #[live]
    pub matrix: bool,
    #[live]
    pub compact: bool,
    #[live]
    pub category: String,
    #[live]
    pub managed: bool,
}

impl Widget for NoEffectPicker {
    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        self.view.draw_walk(cx, scope, walk)
    }
}

script_mod! {
    use mod.prelude.widgets_internal.*

    mod.widgets.EffectPicker = set_type_default() do #(NoEffectPicker::register_widget(vm)) {
        visible: false width: 0 height: 0
    }
}

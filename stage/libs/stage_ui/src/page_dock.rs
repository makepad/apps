//! The house look of the page docks' splitter (Story's CUT and DECK docks,
//! and each window's chat beside its pages): `mod.widgets.DockSplitter` as
//! a themed `Splitter`. A window registers it after the widget families
//! (whose dock DSL names its own `DockSplitter`) and before its pages.

use makepad_widgets::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*
    let stage = mod.stage

    /// The splitter of the page docks: the bar's look, in one place.
    mod.widgets.DockSplitter = Splitter{size: 8
        draw_bg +: {color_bg: stage.bg color: stage.border_hi color_hover: stage.accent color_drag: stage.accent splitter_pad: 3 bar_size: 24}}
}

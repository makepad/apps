//! `FabLibraryFloat`: the Circuit's Library (v9/v10) as Fab's floating panel over the sheet, the
//! explorer's `library` slot (`fab_elec_view::explorer::tools` names its parts). Open, it is a
//! `FabFloat` down the sheet's edge: a LIBRARY header with swap and fold buttons over the
//! electronics library panel (`fab_elec_view::library`). Folded, it is a small tab in the same
//! corner. The pane places it and answers its buttons; this module is its look only.

use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.prelude.fab.*
    use mod.widgets.*

    mod.widgets.FabLibraryFloat = View{
        width: Fit
        height: Fit
        lib_open := mod.widgets.FabFloat{
            new_batch: true
            width: 248
            height: Fill
            flow: Down
            padding: Inset{left: 0 right: 0 top: 0 bottom: 4}
            a11y: {role: "pane" label: "Library"}
            head := View{
                width: Fill
                height: 26
                flow: Right
                align: Align{y: 0.5}
                padding: Inset{left: 10 right: 3}
                spacing: 0
                mod.widgets.FabLabelSmall{ text: "LIBRARY" }
                View{ width: Fill height: 1 }
                mod.widgets.FabTip{ text: "Move the Library to the sheet's other side"
                    lib_side := mod.widgets.FabIconButton{ width: 20 height: 20 a11y: {label: "Move the Library to the other side"} draw_icon +: { svg: crate_resource("self://resources/icons/swap_sides.svg") } }
                }
                mod.widgets.FabTip{ text: "Fold the Library to a tab"
                    lib_fold := mod.widgets.FabIconButton{ width: 20 height: 20 a11y: {label: "Fold the Library"} draw_icon +: { svg: crate_resource("self://resources/icons/chevron_right.svg") } }
                }
            }
            library := mod.widgets.LibraryPanel{}
        }
        lib_tab := mod.widgets.FabFloat{
            visible: false
            new_batch: true
            width: 96
            height: 26
            padding: 2
            mod.widgets.FabTip{ text: "Open the Library (A searches it)"
                lib_unfold := mod.widgets.FabFlatButton{
                    width: Fill
                    height: Fill
                    text: "LIBRARY"
                    a11y: {label: "Open the Library"}
                    icon_walk: Walk{ width: 12 height: 12 }
                    draw_icon +: { svg: crate_resource("self://resources/icons/chevron_left.svg") }
                    draw_text +: { color: fab.color_text_dim text_style: theme.font_regular{ font_size: fab.font_size_small } }
                }
            }
        }
    }
}

use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.DocumentsPage = SolidView{
        width: Fill height: Fill flow: Down draw_bg.color: palette.bg
        doc_error := BodyNote{visible: false padding: 10 draw_text.color: palette.warn}
        markdown := MarkdownEditor{
            host_history: true padding: Inset{top: 10}
            list +: {Row +: {font_color: palette.text draw_text.color: palette.text link_color: palette.accent}}
        }
    }
}

use makepad_widgets::*;

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.DocumentsPage = View{
        width: Fill height: Fill flow: Right
        SolidView{
            width: 153 height: Fill flow: Down padding: 10 spacing: 8 draw_bg.color: palette.panel
            ProjectHeading{text: "Documents"}
            new_doc := PanelButton{width: Fill text: "New document"}
            title_prompt := View{
                visible: false width: Fill height: Fit flow: Down spacing: 4
                doc_title_input := PanelInput{empty_text: "Document title" a11y: {label: "Document title"}}
                View{
                    width: Fill height: Fit flow: Right{wrap: true} spacing: 4
                    add_doc := PanelButton{text: "Add"}
                    cancel_doc := PanelButton{text: "Cancel"}
                }
            }
            PanelLine{}
            document_list := mod.widgets.DocumentListBase{
                width: Fill height: Fill
                list := PortalList{
                    width: Fill height: Fill
                    Row := View{
                        width: Fill height: Fit flow: Down padding: Inset{bottom: 8} spacing: 3
                        open_doc := PanelButton{
                            width: Fill align: Align{x: 0 y: 0.5}
                            draw_text.text_style: theme.font_bold{font_size: 9.75}
                        }
                        edited_by := BodyNote{}
                    }
                }
            }
        }
        PanelLine{width: 1 height: Fill}
        SolidView{
            width: Fill height: Fill flow: Down draw_bg.color: palette.bg
            doc_error := BodyNote{visible: false padding: 10 draw_text.color: palette.warn}
            docs_empty := View{
                width: Fill height: Fill align: Center padding: 14
                BodyNote{width: Fit text: "No documents in this project yet."}
            }
            doc_sheet := View{
                visible: false width: Fill height: Fill flow: Down
                doc_heading := ProjectHeading{width: Fill padding: 12}
                PanelLine{}
                markdown := MarkdownEditor{
                    host_history: true padding: Inset{top: 10}
                    list +: {Row +: {font_color: palette.text draw_text.color: palette.text link_color: palette.accent}}
                }
            }
        }
    }
}

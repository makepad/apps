use crate::lists::{NavigationList, ViewData};
use makepad_widgets::*;

#[derive(Script, ScriptHook, Widget)]
struct ProjectNavigation {
    #[source] source: ScriptObjectRef,
    #[deref] navigation: NavigationList,
    #[rust] remove: Option<WidgetRef>,
    #[rust] undo_row: Option<WidgetRef>,
}

fn show_control(cx: &mut Cx, previous: &mut Option<WidgetRef>, next: Option<WidgetRef>) {
    if *previous != next {
        if let Some(control) = previous { control.set_visible(cx, false); }
        *previous = next;
    }
    if let Some(control) = previous { control.set_visible(cx, true); }
}

impl Widget for ProjectNavigation {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let mut focus = None;
        if let Some(data) = scope.data.get_mut::<ViewData>() {
            let ws = &data.workspace;
            let list = self.navigation.portal_list(cx, ids!(list));
            let (header, existed) = list.item_with_existed(cx, 0, id!(Header));
            header.button(cx, ids!(new_project)).set_enabled(cx, ws.writable);
            let input = header.text_input(cx, ids!(new_project_name));
            if ws.project_name_draft.is_some() && !input.visible() { focus = Some(input.clone()); }
            input.set_visible(cx, ws.project_name_draft.is_some());
            if let Some(name) = ws.project_name_draft.as_ref().filter(|_| !existed) { input.set_text(cx, name); }
            let remove = ws.removable.and_then(|id| ws.project_index(id)).map(|i|
                list.item(cx, i + 1, id!(Project)).widget(cx, ids!(remove_project)));
            show_control(cx, &mut self.remove, remove);

            // The Undo occupies the gap immediately above the next live project (or the footer).
            let undo_row = ws.removed.as_ref().map(|r| {
                let i = r.index.min(ws.projects.len());
                let template = if i == ws.projects.len() { id!(Footer) } else { id!(Project) };
                let row = list.item(cx, i + 1, template).widget(cx, ids!(removed_row));
                row.label(cx, ids!(removed_title)).set_text(cx, &r.title);
                row
            });
            show_control(cx, &mut self.undo_row, undo_row);
        }
        let step = self.navigation.draw_walk(cx, scope, walk);
        // A newly opened field gets its focusable area on its first draw.
        if let Some(input) = focus { input.set_key_focus(cx); }
        step
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.navigation.handle_event(cx, event, scope);
    }
}

script_mod! {
    use mod.prelude.workspace.*
    use mod.widgets.*

    mod.widgets.RailText = Label{
        padding: 0 draw_text +: {color: palette.text text_style: theme.font_bold{font_size: 9.75}}
    }
    mod.widgets.Kicker = Label{
        padding: 0 draw_text +: {color: palette.faint text_style: theme.font_code{font_size: 7.875}}
    }
    let RailButton = PanelButton{
        width: Fill height: 30 align: Align{x: 0 y: 0.5}
        padding: Inset{left: 10 right: 10}
        draw_bg +: {border_size: 0 border_radius: 3.5 color: palette.panel}
        draw_text +: {color: palette.muted text_style: theme.font_bold{font_size: 9.75}}
        icon_walk: Walk{width: 14 height: 14}
        draw_icon +: {color: palette.muted color_hover: palette.text}
    }
    let Coming = Tip{text: "coming" control := RailButton{enabled: false}}
    let Rule = PanelLine{margin: Inset{top: 8 bottom: 8}}
    let ActiveMark = SolidView{width: 2 height: Fill draw_bg.color: palette.accent}
    let TabRow = View{
        width: Fill height: 30 flow: Overlay
        target := RailButton{}
        marker := ActiveMark{visible: false}
    }
    let Removed = View{
        visible: false width: Fill height: 32 flow: Right align: Align{y: 0.5} padding: Inset{left: 2}
        removed_title := RailText{width: Fill max_lines: 1 text_overflow: Ellipsis draw_text.text_style: theme.font_regular{font_size: 8.5}}
        RailText{text: " removed ·" draw_text.text_style: theme.font_regular{font_size: 8.5}}
        undo_project := RailButton{
            width: 32 height: 25 text: "Undo" padding: 0 align: Center
            draw_text +: {color: palette.accent text_style: theme.font_bold{font_size: 8.5}} a11y: {label: "Undo project removal"}
        }
    }
    mod.widgets.SideNavigation = #(ProjectNavigation::register_widget(vm)){
        width: Fill height: Fill
        list := PortalList{
            width: Fill height: Fill
            Header := View{
                width: Fill height: Fit flow: Down spacing: 2
                Kicker{text: "SPACE"}
                space_menu := DropDown{width: Fill a11y: {label: "Space"} draw_text +: {text_overflow: TextOverflow.Ellipsis}}
                talk_tab := TabRow{target +: {text: "Talk" draw_icon.svg: crate_resource("self://resources/icons/chat.svg")}}
                Rule{}
                View{
                    width: Fill height: Fit flow: Right align: Align{y: 0.5}
                    projects := RailButton{
                        height: 25 text: "PROJECTS"
                        draw_text +: {color: palette.faint text_style: theme.font_code{font_size: 7.875}}
                    }
                    new_project := RailButton{
                        width: 26 height: 25 padding: 0 align: Center text: "+"
                        a11y: {label: "New project"}
                    }
                }
                new_project_name := PanelInput{
                    visible: false width: Fill empty_text: "Project name" a11y: {label: "Project name"}
                }
            }
            Project := View{
                width: Fill height: Fit flow: Down
                removed_row := Removed{}
                View{
                    width: Fill height: 32 padding: Inset{top: 1 bottom: 1} flow: Overlay
                    select_project := RailButton{text: "" a11y: {label: "Open project"}}
                    project_mark := ActiveMark{visible: false}
                    View{
                        width: Fill height: Fill flow: Right align: Align{y: 0.5}
                        padding: Inset{left: 9 right: 5} spacing: 4
                        RoundedView{width: 9 height: 9 draw_bg +: {color: palette.accent border_radius: 1.5}}
                        name := RailText{width: Fill max_lines: 1 text_overflow: Ellipsis}
                        RoundedView{
                            width: Fit height: 17 padding: Inset{left: 7 right: 7} align: Center
                            draw_bg +: {color: palette.panel2 border_radius: 5}
                            count := Kicker{draw_text.color: palette.muted}
                        }
                        remove_project := RailButton{
                            visible: false width: 20 height: 23 padding: 0 align: Center text: "−"
                            a11y: {label: "Remove empty project"}
                        }
                    }
                }
            }
            Footer := View{
                width: Fill height: Fit flow: Down spacing: 2
                removed_row := Removed{}
                Rule{}
                section := Kicker{width: Fill padding: Inset{left: 10 right: 10 top: 6 bottom: 4}}
                tasks_tab := TabRow{target +: {text: "Tasks" draw_icon.svg: crate_resource("self://resources/icons/tasks.svg")}}
                board_tab := TabRow{target +: {text: "Board" draw_icon.svg: crate_resource("self://resources/icons/board.svg")}}
                docs_tab := TabRow{target +: {text: "Docs" draw_icon.svg: crate_resource("self://resources/icons/docs.svg")}}
                share_tab := TabRow{target +: {text: "Share" draw_icon.svg: crate_resource("self://resources/icons/share.svg")}}
                Rule{}
                Coming{control +: {text: "Voice notes"}}
                Coming{control +: {text: "Clips"}}
                Coming{control +: {text: "Shots"}}
                Coming{control +: {text: "Commits"}}
            }
        }
    }
    let IconTab = Tip{
        button := PanelButton{
            width: 32 height: 28 padding: 0 text: ""
            draw_bg +: {color: palette.panel border_size: 0}
            icon_walk: Walk{width: 16 height: 16}
            draw_icon +: {color: palette.muted color_hover: palette.text}
        }
    }
    mod.widgets.CompactNavigation = SolidView{
        visible: false width: Fill height: Fit flow: Right{wrap: true} spacing: 4
        padding: Inset{left: 10 right: 10 top: 4 bottom: 4} draw_bg.color: palette.panel
        compact_space := DropDown{width: Fit a11y: {label: "Space"}}
        tasks_icon := IconTab{text: "Tasks" button +: {a11y: {label: "Tasks"} draw_icon.svg: crate_resource("self://resources/icons/tasks.svg")}}
        board_icon := IconTab{text: "Board" button +: {a11y: {label: "Board"} draw_icon.svg: crate_resource("self://resources/icons/board.svg")}}
        docs_icon := IconTab{text: "Docs" button +: {a11y: {label: "Docs"} draw_icon.svg: crate_resource("self://resources/icons/docs.svg")}}
        share_icon := IconTab{text: "Share" button +: {a11y: {label: "Share"} draw_icon.svg: crate_resource("self://resources/icons/share.svg")}}
        projects_icon := IconTab{text: "Projects" button +: {a11y: {label: "Projects"} draw_icon.svg: crate_resource("self://resources/icons/projects.svg")}}
        chat_icon := IconTab{text: "Talk" button +: {a11y: {label: "Talk"} draw_icon.svg: crate_resource("self://resources/icons/chat.svg")}}
    }

}

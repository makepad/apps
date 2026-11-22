//! The Model sources editor (Edit › 3D Model Sources…, Help › External Resources and Licences…
//! show it alone over the workspace, with Back; any area can hold it too): three pages. Settings: a switch per source of part models, with its state.
//! EasyEDA's warning: shown when its switch is turned on; it turns on only by its Turn on button.
//! Licences: every external resource, what Fab takes from it, from where, under which licence,
//! whether it is on and when it was last used ([`crate::model_sources::resources`]), with the
//! firmware network's projects and tokens before the built-in data
//! ([`crate::firmware_net::resource`]).
//!
//! Cost: per draw, the shown page and choices compared with the state's (no copy); the texts are
//! set, and the choices copied, only when they differ. Nothing while off screen.

use crate::api::*;
use crate::ui::editors::Editor;
use crate::firmware_net::{FirmwareNet, TokenEntry};
use crate::model_sources::{date, resources, ModelSource, ModelSources, EASYEDA_WARNING, EASYEDA_WARNING_TITLE};
use makepad_widgets_core::*;

script_mod! {
    use mod.prelude.fab.*
    use mod.widgets.*

    let Wrapped = mod.widgets.FabLabel{ width: Fill height: Fit flow: Right{wrap: true} }
    let Note = mod.widgets.FabLabelMuted{ width: Fill height: Fit flow: Right{wrap: true} margin: Inset{left: 26} }
    let Heading = mod.widgets.FabHeaderLabel{ width: Fill height: Fit margin: Inset{top: 6} }
    let Buttons = View{ width: Fill height: Fit flow: Right spacing: 8 align: Align{x: 0.0 y: 0.5} margin: Inset{top: 6} }
    let Page = View{ width: Fill height: Fit flow: Down spacing: 6 }

    mod.widgets.FabModelSourcesBase = #(FabModelSources::register_widget(vm))
    mod.widgets.FabModelSources = set_type_default() do mod.widgets.FabModelSourcesBase{
        width: Fill
        height: Fill
        flow: Down
        show_bg: true
        draw_bg +: { color: fab.color_editor }
        // Back, while the editor is shown alone over the workspace (Edit › 3D Model Sources…)
        head := View{
            visible: false
            width: Fill height: Fit
            padding: Inset{left: 10 right: 10 top: 8 bottom: 4}
            close := mod.widgets.FabButton{ text: "‹ Back" a11y: {label: "Back to the workspace"} }
        }
        body := mod.widgets.FabScroll{
            align: Align{x: 0.5}
            View{
                // a readable measure when the editor fills the window
                width: Fill{max: 680.}
                height: Fit
                flow: Down
                padding: Inset{left: 14 right: 14 top: 10 bottom: 14}
                settings := Page{
                    mod.widgets.FabTitleLabel{ text: "3D Model Sources" }
                    Wrapped{ text: "Where Fab may get the 3D models of electronics parts. Fab keeps what it gets in your own part library; none of these models is shipped with Fab." }
                    mod.widgets.FabHr{}
                    kicad_local := mod.widgets.FabCheckBox{ text: "KiCad library, from a KiCad installed on this computer" a11y: {label: "KiCad library from this computer"} }
                    kicad_local_note := Note{}
                    kicad_download := mod.widgets.FabCheckBox{ text: "KiCad library, downloaded one model at a time from KiCad's repository" a11y: {label: "KiCad library downloads"} }
                    kicad_download_note := Note{}
                    easyeda := mod.widgets.FabCheckBox{ text: "EasyEDA part service (easyeda.com)" a11y: {label: "EasyEDA part service"} }
                    easyeda_note := Note{}
                    Buttons{
                        licences := mod.widgets.FabButton{ text: "External Resources and Licences" }
                    }
                }
                warning := Page{
                    visible: false
                    warning_title := mod.widgets.FabTitleLabel{ text: "" }
                    warning_text := Wrapped{ text: "" }
                    Buttons{
                        Filler{}
                        cancel := mod.widgets.FabButton{ text: "Cancel" }
                        accept := mod.widgets.FabButtonAccent{ text: "Turn on" }
                    }
                }
                licences_page := Page{
                    visible: false
                    mod.widgets.FabTitleLabel{ text: "External Resources and Licences" }
                    r0 := Heading{ text: "" }
                    r0_text := Wrapped{ text: "" }
                    r1 := Heading{ text: "" }
                    r1_text := Wrapped{ text: "" }
                    r2 := Heading{ text: "" }
                    r2_text := Wrapped{ text: "" }
                    r3 := Heading{ text: "" }
                    r3_text := Wrapped{ text: "" }
                    r4 := Heading{ text: "" }
                    r4_text := Wrapped{ text: "" }
                    r5 := Heading{ text: "" }
                    r5_text := Wrapped{ text: "" }
                    Buttons{
                        back := mod.widgets.FabButton{ text: "3D Model Sources" }
                    }
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct FabModelSources {
    #[deref]
    view: View,
    /// The page shown and the choices its texts were made from.
    #[rust]
    shown: Option<(SourcesPage, ModelSources, FirmwareNet, Result<Vec<TokenEntry>, String>)>,
}

/// What the settings page says under a source's switch.
fn note(sources: &ModelSources, source: ModelSource) -> String {
    let used = sources.last_used(source).map_or(String::new(), |at| format!(" Last used {}.", date(at)));
    match source {
        ModelSource::KicadLocal => format!("Reads the 3D models of KiCad's library from its folder; no network.{used}"),
        ModelSource::KicadDownload => format!(
            "Downloads only the model files your boards use, from {} (KiCad's library repository), else {}; each is kept with its library tag.{used}",
            fab_elec_import::kicad3d::GITLAB_HOST, fab_elec_import::kicad3d::GITHUB_HOST),
        ModelSource::EasyEda => match (sources.easyeda, sources.easyeda_accepted) {
            (true, Some(at)) => format!("On since you accepted its terms on {}. Its models come under the service's own terms.{used}", date(at)),
            (_, Some(at)) => format!("Off. You accepted its terms on {}; turning it on asks again.{used}", date(at)),
            _ => format!("Off. Turning it on shows its terms, which you accept before Fab uses it.{used}"),
        },
    }
}

impl FabModelSources {
    fn show(&mut self, cx: &mut Cx, page: SourcesPage, sources: &ModelSources, net: &FirmwareNet, tokens: &Result<Vec<TokenEntry>, String>) {
        self.view.view(cx, ids!(settings)).set_visible(cx, page == SourcesPage::Settings);
        self.view.view(cx, ids!(warning)).set_visible(cx, page == SourcesPage::EasyEdaWarning);
        self.view.view(cx, ids!(licences_page)).set_visible(cx, page == SourcesPage::Licences);
        for (id, source) in [(ids!(kicad_local), ModelSource::KicadLocal), (ids!(kicad_download), ModelSource::KicadDownload),
            (ids!(easyeda), ModelSource::EasyEda)]
        {
            self.view.check_box(cx, id).set_active(cx, sources.on(source), Animate::No);
        }
        self.view.label(cx, ids!(kicad_local_note)).set_text(cx, &note(sources, ModelSource::KicadLocal));
        self.view.label(cx, ids!(kicad_download_note)).set_text(cx, &note(sources, ModelSource::KicadDownload));
        self.view.label(cx, ids!(easyeda_note)).set_text(cx, &note(sources, ModelSource::EasyEda));
        self.view.label(cx, ids!(warning_title)).set_text(cx, EASYEDA_WARNING_TITLE);
        self.view.label(cx, ids!(warning_text)).set_text(cx, EASYEDA_WARNING);
        let mut rows = resources(sources);
        rows.insert(rows.len() - 1, crate::firmware_net::resource(net, tokens));
        for (i, (title, text)) in [(ids!(r0), ids!(r0_text)), (ids!(r1), ids!(r1_text)),
            (ids!(r2), ids!(r2_text)), (ids!(r3), ids!(r3_text)), (ids!(r4), ids!(r4_text)),
            (ids!(r5), ids!(r5_text))]
            .into_iter().enumerate()
        {
            let row = rows.get(i);
            self.view.label(cx, title).set_text(cx, row.map_or("", |r| r.title.as_str()));
            self.view.label(cx, text).set_text(cx, &row.map_or(String::new(), |r| r.lines.join("\n")));
        }
    }
}

impl Widget for FabModelSources {
    fn semantics(&self, _cx: &Cx, node: &mut a11y_tree::Node) {
        node.role = a11y_tree::Role::Group;
        makepad_platform::a11y::set_string(&mut node.name, "3D model sources");
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        if !scope.data.get::<AppState>().is_some_and(|state| state.ui.on_screen(Editor::ModelSources)) {
            return;
        }
        self.view.handle_event(cx, event, scope);
        let Event::Actions(actions) = event else { return };
        if self.view.button(cx, ids!(close)).clicked(actions) {
            cx.action(ShellAction::ShowModelSources(None));
            return;
        }
        for (id, source) in [(ids!(kicad_local), ModelSource::KicadLocal), (ids!(kicad_download), ModelSource::KicadDownload),
            (ids!(easyeda), ModelSource::EasyEda)]
        {
            if let Some(on) = self.view.check_box(cx, id).changed(actions) {
                cx.action(ShellAction::SetModelSource(source, on));
            }
        }
        if self.view.button(cx, ids!(licences)).clicked(actions) {
            cx.action(ShellAction::ModelSourcesPage(SourcesPage::Licences));
        }
        if self.view.button(cx, ids!(back)).clicked(actions) || self.view.button(cx, ids!(cancel)).clicked(actions) {
            cx.action(ShellAction::ModelSourcesPage(SourcesPage::Settings));
        }
        if self.view.button(cx, ids!(accept)).clicked(actions) {
            cx.action(ShellAction::AcceptEasyEda);
        }
    }

    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        if let Some(state) = scope.data.get::<AppState>() {
            self.view.view(cx, ids!(head)).set_visible(cx, state.ui.page == Some(Editor::ModelSources));
            let same = self.shown.as_ref().is_some_and(|(page, sources, net, tokens)| {
                *page == state.ui.sources_page && *sources == state.model_sources && *net == state.firmware_net && *tokens == state.firmware_tokens
            });
            if !same {
                self.show(cx, state.ui.sources_page, &state.model_sources, &state.firmware_net, &state.firmware_tokens);
                self.shown = Some((state.ui.sources_page, state.model_sources.clone(), state.firmware_net.clone(), state.firmware_tokens.clone()));
            }
        }
        self.view.draw_walk(cx, scope, walk)
    }
}

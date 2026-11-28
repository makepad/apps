use super::{diff_fn_edits, format_fn_sources, ShaderLayer, ShaderTab};
use crate::tweak::{panel::set_tab_fill, props::view::Sidebar, Request, RequestId, Session};
use makepad_widgets::*;
use std::{collections::BTreeMap, rc::Rc};

/// A FlatList yields its draw step to the parent. Keep its buttons in reflection order.
#[derive(Script, ScriptHook, Widget)]
pub struct ShaderPills {
    #[source] source: ScriptObjectRef,
    #[deref] view: View,
    #[rust] buttons: Vec<WidgetRef>,
}

impl ShaderPills {
    fn bind(&mut self, cx: &mut Cx, names: &[String], active: &str) {
        let pills = self.view.flat_list(cx, ids!(shader_layers));
        let Some(mut pills) = pills.borrow_mut() else { return };
        self.buttons.clear();
        for name in names {
            let Some(btn) = pills.item(cx, LiveId::from_str(name), id!(Pill)) else { continue };
            btn.set_text(cx, name);
            set_tab_fill(cx, btn.clone(), name == active);
            self.buttons.push(btn);
        }
        pills.items.retain_visible();
        pills.redraw(cx);
    }
}

impl Widget for ShaderPills {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(step) = self.view.draw_walk(cx, scope, walk).step() {
            if step.borrow::<FlatList>().is_none() { return Err(step); }
            for button in &self.buttons { button.draw_all(cx, scope); }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

impl ShaderTab {
    pub(in crate::tweak) fn sync<'a>(&mut self, cx: &mut Cx, root: &WidgetRef, sidebar: &'a Sidebar) -> Option<&'a ShaderLayer> {
        let layers = &sidebar.props.shaders;
        let layer = layers.iter().find(|l| l.layer == self.active_layer).or_else(|| layers.first());
        let name = layer.map_or("", |l| l.layer.as_str());
        root.widget(cx, ids!(shader_pill_bar)).set_visible(cx, layer.is_some());
        let key_changed = self.synced_key.0 != sidebar.path || self.synced_key.1 != name;
        let pills_changed = self.layer_pills.iter().map(String::as_str).ne(layers.iter().map(|l| l.layer.as_str()));
        if key_changed || pills_changed {
            self.active_layer = name.to_owned();
            self.layer_pills = layers.iter().map(|l| l.layer.clone()).collect();
            if let Some(mut bar) = root.widget(cx, ids!(shader_pill_bar)).borrow_mut::<ShaderPills>() {
                bar.bind(cx, &self.layer_pills, name);
            }
        }
        root.label(cx, ids!(shader_title)).set_text(cx, name);
        let doc = match layer {
            Some(l) if !l.drawn => format!("{name} is not drawn: no live draw call (show_bg off?)"),
            Some(l) => l.doc.clone(),
            None if !sidebar.path.is_empty() => "No shader layers on this widget.".into(),
            None => String::new(),
        };
        root.label(cx, ids!(shader_doc)).set_text(cx, &doc);
        let editable = layer.is_some_and(|l| !l.fns.is_empty());
        let src = root.text_input(cx, ids!(shader_src));
        if src.is_read_only() == editable { src.set_is_read_only(cx, !editable); }
        if key_changed || !cx.has_key_focus(src.area()) {
            let shown = src.text();
            if key_changed || shown == self.synced_text {
                let text = layer.map(format_fn_sources).unwrap_or_default();
                if shown != text { src.set_text(cx, &text); }
                self.synced_text = text;
            }
        }
        if key_changed {
            self.synced_key = (sidebar.path.clone(), name.to_owned());
            self.status.clear();
        }
        root.label(cx, ids!(shader_src_status)).set_text(cx, &self.status);
        root.widget(cx, ids!(src_fold)).set_visible(cx, layer.is_some());
        root.button(cx, ids!(src_fold)).set_text(cx, if self.src_open { "- source" } else { "+ source" });
        src.set_visible(cx, self.src_open && layer.is_some());
        root.widget(cx, ids!(shader_propose)).set_visible(cx, self.src_open && editable);
        layer
    }

    pub(in crate::tweak) fn actions<L: Copy + Eq, T: Copy + Eq>(&mut self, cx: &mut Cx, root: &WidgetRef,
        actions: &Actions, sidebar: &Sidebar, session: &mut Session<L, T>,
        proposals: &mut BTreeMap<RequestId, (Rc<str>, String)>) -> bool {
        for (_, button) in root.flat_list(cx, ids!(shader_layers)).items_with_actions(actions) {
            if button.as_button().clicked(actions) {
                self.active_layer = button.text();
                return true;
            }
        }
        if root.button(cx, ids!(src_fold)).clicked(actions) {
            self.src_open = !self.src_open;
            return true;
        }
        let src = root.text_input(cx, ids!(shader_src));
        if !self.src_open || src.is_read_only() { return false; }
        let shortcut = actions.filter_widget_actions(src.widget_uid()).filter_map(|a| a.action.downcast_ref::<TextInputAction>()).any(|a| match a {
            TextInputAction::Returned(_, mods) => mods.control || mods.logo,
            TextInputAction::KeyDownUnhandled(key) => matches!(key.key_code, KeyCode::ReturnKey | KeyCode::NumpadEnter)
                && (key.modifiers.control || key.modifiers.logo),
            _ => false,
        });
        if shortcut || root.button(cx, ids!(shader_propose)).clicked(actions) {
            let Some(layer) = sidebar.props.shaders.iter().find(|l| l.layer == self.active_layer) else { return false };
            let edits = diff_fn_edits(layer, &src.text());
            self.status = if edits.is_empty() { "No changed shader functions." } else { "" }.into();
            for (prop, value) in edits {
                match session.request(None, Request::Set { path: sidebar.path.to_string(), prop, value }) {
                    Ok(id) => { proposals.insert(id, self.synced_key.clone()); }
                    Err(why) => self.status = why,
                }
            }
            root.label(cx, ids!(shader_src_status)).set_text(cx, &self.status);
        }
        false
    }
}

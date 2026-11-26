//! Widget layers: a Makepad UI subtree written in the Splash widget DSL
//! (`ui: fn(props) { View{…} }`), evaluated in Makepad's own VM with the
//! frame's `props` (`props.title`, `props.progress`, `props.accent`), laid out at
//! the layer's size and drawn into its own target, then placed by the
//! layer's transform.
//!
//! The `ui` is evaluated once, as a function of `props`; the tree is built
//! again by calling it whenever the props change (every frame while one is
//! animated) and never receives events, so no animator or timer of its own
//! runs: what a frame shows is a function of the props alone.

use super::MotionDraw;
use makepad_motion::frame::{Item, ItemKind, WidgetValue};
use makepad_widgets::*;
use std::collections::HashMap;

struct Tree {
    key: u64,
    root: WidgetRef,
}

/// A layer's `ui` as a function of `props` (held in a rooted object, so its
/// body lives while the layer does).
struct Ui {
    source: u64,
    holder: ScriptObjectRef,
}

#[derive(Default)]
pub(crate) struct Widgets {
    trees: HashMap<String, Tree>,
    uis: HashMap<String, Ui>,
    errors: Vec<String>,
}

impl Widgets {
    pub fn errors(&self) -> Vec<String> {
        self.errors.clone()
    }

    pub fn handle_event(&mut self, _cx: &mut Cx, _event: &Event) {}
}

/// What a `ui` is adopted under, as a function of `props`.
const UI_SCOPE_FILE: &str = "motion://widget_scope";
const UI_PRELUDE: &str = "use mod.prelude.widgets.*\nnil\n";

/// A layer's `ui` (the document's fn) as a fn of this VM, moved here by its
/// tokens under the widget prelude.
pub fn widget_ui(vm: &mut ScriptVm, ui: &makepad_render_graph::pass::PassFn) -> Result<ScriptValue, String> {
    let scope = match vm.end_scope_of(UI_SCOPE_FILE) {
        Some(scope) => scope,
        None => {
            vm.capture_errors();
            let v = vm.eval(ScriptMod::new("", "", UI_SCOPE_FILE, 0, 0, UI_PRELUDE, &[]));
            take_errors(vm, v, "widget prelude")?;
            vm.end_scope_of(UI_SCOPE_FILE).ok_or("the widget prelude did not evaluate")?
        }
    };
    vm.capture_errors();
    let f = vm.adopt_fn(&ui.image, scope);
    let errors = vm.take_errors();
    if !errors.is_empty() {
        return Err(errors.join("; ").replace(&format!("adopted://{:016x}", ui.image.hash()), &ui.at.file));
    }
    f.map(|f| f.into()).ok_or_else(|| "the ui is not a function".to_string())
}

/// A hash of the props' values (the tree's rebuild key).
fn props_key(props: &[(String, WidgetValue)]) -> u64 {
    let mut id = LiveId(0);
    for (name, value) in props {
        id = id.str_append(name);
        id = match value {
            WidgetValue::Num(n) => id.bytes_append(&n.to_bits().to_le_bytes()),
            WidgetValue::Color(c) => c.iter().fold(id, |id, v| id.bytes_append(&v.to_bits().to_le_bytes())),
            WidgetValue::Text(text) => id.str_append(text),
        };
    }
    id.0
}

/// `props` as a Splash object.
fn props_object(vm: &mut ScriptVm, props: &[(String, WidgetValue)]) -> ScriptValue {
    let object = vm.bx.heap_mut().new_object();
    for (name, value) in props {
        let value = match value {
            WidgetValue::Num(n) => (*n).into(),
            WidgetValue::Color(c) => {
                let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
                ScriptValue::from_color(b(c[0]) << 24 | b(c[1]) << 16 | b(c[2]) << 8 | b(c[3]))
            }
            WidgetValue::Text(text) => vm.new_string_with(|_, s| s.push_str(text)),
        };
        vm.bx.heap_mut().set_value_def(object, LiveId::from_str(name).into(), value);
    }
    object.into()
}

fn take_errors(vm: &mut ScriptVm, value: ScriptValue, what: &str) -> Result<(), String> {
    let errors = vm.take_errors();
    if value.is_err() || !errors.is_empty() {
        return Err(if errors.is_empty() { format!("the {what} did not evaluate") } else { errors.join("; ") });
    }
    Ok(())
}

/// Build (when the props changed) and draw a widget item into its own
/// target at `scale` pixels per unit.
pub(super) fn render(md: &mut MotionDraw, cx: &mut Cx2d, item: &Item, scale: f64) -> Option<Texture> {
    let ItemKind::Widget { ui: Some(ui), size, props } = &item.kind else { return None };
    let source = ui.image.hash();
    let key = LiveId(source).bytes_append(&props_key(props).to_le_bytes()).0;
    if md.widgets.trees.get(&item.path).is_none_or(|tree| tree.key != key) {
        let widgets = &mut md.widgets;
        let built: Result<WidgetRef, String> = cx.with_vm(|vm| {
            vm.capture_errors();
            if widgets.uis.get(&item.path).is_none_or(|u| u.source != source) {
                let f = widget_ui(vm, ui)?;
                let holder = vm.bx.heap_mut().new_object();
                vm.bx.heap_mut().set_value_def(holder, id!(ui).into(), f);
                widgets.uis.insert(item.path.clone(), Ui { source, holder: vm.bx.heap_mut().new_object_ref(holder) });
            }
            let holder = widgets.uis[&item.path].holder.as_object();
            let f = vm.bx.heap_ref().value(holder, id!(ui).into(), NoTrap);
            let props = props_object(vm, props);
            let value = vm.call(f, &[props]);
            take_errors(vm, value, "ui")?;
            Ok(WidgetRef::script_from_value(vm, value))
        });
        match built {
            Ok(root) => {
                md.widgets.trees.insert(item.path.clone(), Tree { key, root });
            }
            Err(error) => {
                let error = format!("{}: ui: {error}", item.path);
                if !md.widgets.errors.contains(&error) {
                    md.widgets.errors.push(error);
                }
                return None;
            }
        }
    }
    let px = ((size[0] * scale).ceil().max(1.0) as u32, (size[1] * scale).ceil().max(1.0) as u32);
    md.note_kind(&format!("widget {key:016x}"));
    let index = md.target(cx, px, "motion_widget");
    let target = &mut md.pool[index];
    // Laid out in document units, rasterised at `scale`.
    let logical = dvec2(size[0].max(1.0), size[1].max(1.0));
    target.pass.set_size(cx, logical);
    target.pass.set_color_texture(cx, &target.texture, DrawPassClearColor::ClearWith(vec4(0.0, 0.0, 0.0, 0.0)));
    cx.make_child_pass(&target.pass);
    cx.begin_pass(&target.pass, Some(scale.max(1e-3)));
    target.list.begin_always(cx);
    cx.begin_root_turtle(logical, Layout::flow_down());
    if let Some(tree) = md.widgets.trees.get_mut(&item.path) {
        let _ = tree.root.draw_walk(cx, &mut Scope::empty(), Walk::fixed(logical.x, logical.y));
    }
    let target = &mut md.pool[index];
    cx.end_pass_sized_turtle();
    target.list.end(cx);
    cx.end_pass(&target.pass);
    let id = target.pass.draw_pass_id();
    md.record(id);
    Some(md.pool[index].texture.clone())
}

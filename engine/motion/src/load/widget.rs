//! The Widget layer, read from its component.

use super::{pack, At, Reader, Slot};
use crate::layers::Widget;
use crate::model::*;
use crate::obj::Look;

impl Widget {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        // `ui: fn(props) { View{…} }`: the widget tree as a function of the
        // props, moved to the widget host (Makepad's own VM) by its tokens.
        let hint = "`ui: fn(props) { View{flow: Down Label{text: props.title}} }` with `props: {title: \"…\"}`";
        let (ui, ui_loc) = match self.ui.look(c) {
            Some(Look::Fn(f)) => match c.fn_code(f) {
                Some((loc, code)) => (Some(code), loc.unwrap_or_else(|| c.loc_of(at, "ui"))),
                None => {
                    c.err(at, "ui", "the `ui` function could not be read".into(), None);
                    (None, at.loc)
                }
            },
            Some(other) => {
                c.err(at, "ui", format!("`ui` is a function building the widget tree in the Splash widget DSL, not {}", other.kind()), Some(hint));
                (None, at.loc)
            }
            None => {
                c.err(at, "ui", "a Widget needs `ui: fn(props) { View{ … } }`".into(), Some(hint));
                (None, at.loc)
            }
        };
        let size = if self.size.is_set() {
            self.size.prop(c, at, "size", [0.0, 0.0])
        } else {
            c.err(at, "size", "a Widget is laid out at its `size`".into(), Some("`size: vec2(800, 450)`"));
            Prop::Const([0.0, 0.0])
        };
        let mut props = Vec::new();
        if let Some(value) = self.props.look(c) {
            match &value {
                Look::Obj(pobj, _) => {
                    let pat = c.at(*pobj, at.field("props"), at.loc);
                    for (name, value) in c.fields(*pobj) {
                        let value = match c.slot(&value) {
                            Slot::Input(input) => match c.input(&pat, &name, &input) {
                                Some(InputValue::Text(text)) => Look::Str(text),
                                Some(InputValue::Number(n)) => Look::Num(n),
                                Some(InputValue::Color(col)) => Look::Color(pack(col)),
                                _ => continue,
                            },
                            Slot::Value(value) => value.clone(),
                        };
                        let prop = match &value {
                            Look::Str(text) => WidgetProp::Text(text.clone()),
                            Look::Color(_) => WidgetProp::Color(c.prop_value::<4>(&pat, &name, &value).unwrap_or(Prop::Const([1.0; 4]))),
                            Look::Obj(o, _) if c.get(*o, "color").is_some() => WidgetProp::Color(c.prop_value::<4>(&pat, &name, &value).unwrap_or(Prop::Const([1.0; 4]))),
                            Look::Obj(..) if value.anim().is_some() && c.anim_is_color(&value) => WidgetProp::Color(c.prop_value::<4>(&pat, &name, &value).unwrap_or(Prop::Const([1.0; 4]))),
                            _ => WidgetProp::Num(c.prop_value::<1>(&pat, &name, &value).unwrap_or(Prop::Const([0.0]))),
                        };
                        props.push((name, prop));
                    }
                }
                other => c.err(at, "props", format!("`props` is an object of name: value the UI reads as `props.<name>`, not {}", other.kind()), None),
            }
        }
        LayerKind::Widget { ui, ui_loc, size, props }
    }
}

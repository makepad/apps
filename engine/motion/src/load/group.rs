//! The structural layers: Group, Repeat, Freeze, FlyThrough, Draw2D,
//! read from their components.

use super::{check_path_data, did_you_mean, At, Reader};
use crate::layers::{Draw2D, FlyThrough, Freeze, Group, Repeat};
use crate::model::*;
use crate::obj::Look;
use crate::value::fmt_num;

impl Group {
    pub(super) fn read(&self, c: &mut Reader, at: &At, path: &str) -> LayerKind {
        let layers = match self.layers.look(c) {
            Some(Look::Arr(items)) => c.layers(at, "layers", &items),
            Some(other) => {
                c.err(at, "layers", format!("a Group's `layers` is an array of layers, not {}", other.kind()), None);
                Vec::new()
            }
            None => {
                c.err(at, "layers", "a Group has `layers: [...]`".into(), None);
                Vec::new()
            }
        };
        let mask = match self.mask.look(c) {
            None => None,
            Some(value) => {
                // `mask_feather: 20` softens the matte's edge: the mask
                // layer's own blur (in document units) unless it sets one.
                let feather = match &value {
                    Look::Obj(..) => self.mask_feather.look(c),
                    _ => None,
                };
                c.layer_with(at, "mask", &format!("{path}.mask"), &value, feather).map(Box::new)
            }
        };
        LayerKind::Group {
            layers,
            mask,
            mask_mode: self.mask_mode.pick(c, at, "mask_mode", MaskMode::Alpha),
            stagger: self.stagger.stagger(c, at, "stagger"),
            size: self.size.size(c, at, "size"),
            post: GroupPost(c.post_passes(at, self.post.0)),
            glyphs: self.glyphs.flag(c, at, "glyphs", false),
        }
    }
}

impl Repeat {
    /// `Repeat{layer: Rect{…} count: 24 layout: @grid|@radial|@along|@line
    /// columns spacing radius start sweep face path delay step jitter seed}`,
    /// or `Repeat{source: in(@rows) layer: Text{text: item(@name)} …}`: one
    /// copy per item of a list, its `item()` / `item(@field)` filled.
    pub(super) fn read(&self, c: &mut Reader, at: &At, path: &str) -> Option<LayerKind> {
        let Some(template_value) = self.layer.look(c) else {
            c.err(at, "layer", "a Repeat copies one `layer: Rect{…}` (any layer, a Group for several)".into(), None);
            return None;
        };
        let source = c.repeat_source(at, self.source.look(c));
        let layer_path = format!("{path}.layer");
        // With a `source`, the document's evaluation made one copy of the
        // layer per item (`crate::lower`), or one with blank items for an
        // empty list.
        let (template, items) = match (&source, &template_value) {
            (Some(len), Look::Arr(copies)) => {
                if *len == 0 {
                    let saved = std::mem::take(&mut *c.diags);
                    let layer = copies.first().and_then(|blank| c.layer(at, "layer", &layer_path, blank));
                    *c.diags = saved;
                    (layer?, Vec::new())
                } else {
                    let mut items = Vec::with_capacity(copies.len());
                    for copy in copies {
                        items.push(c.layer(at, "layer", &layer_path, copy)?);
                    }
                    (items.first()?.clone(), items)
                }
            }
            (Some(_), _) => return None,
            (None, _) => (c.layer(at, "layer", &layer_path, &template_value)?, Vec::new()),
        };
        let count = match &source {
            Some(len) => {
                if self.count.is_set() {
                    c.err(at, "count", "a Repeat with a `source` makes one copy per item; leave `count` out".into(), None);
                }
                *len as f64
            }
            None => self.count.konst(c, at, "count", 1.0),
        };
        if !(count >= 0.0) || count.fract() != 0.0 {
            c.err(at, "count", format!("`count` is a whole number of copies, not {}", fmt_num(count)), None);
        }
        let count = count.max(0.0) as usize;
        let face = self.face.is_true(c);
        let spacing = match self.spacing.look(c) {
            None => [100.0, 100.0],
            Some(Look::Num(n)) => [n, n],
            Some(value) => c.vec2_const(at, "spacing", &value).unwrap_or([100.0, 100.0]),
        };
        let layout_id = match self.layout.look(c) {
            None => "grid".to_string(),
            Some(Look::Id(id)) => id,
            Some(other) => {
                c.err(at, "layout", format!("`layout` is @grid, @radial, @along or @line, not {}", other.kind()), None);
                "grid".into()
            }
        };
        let layout = match layout_id.as_str() {
            "grid" => RepeatLayout::Grid { columns: self.columns.konst(c, at, "columns", (count as f64).sqrt().ceil().max(1.0)).max(1.0) as usize, spacing },
            "radial" => RepeatLayout::Radial {
                radius: self.radius.konst(c, at, "radius", 200.0),
                start: self.start.konst(c, at, "start", 0.0),
                sweep: self.sweep.konst(c, at, "sweep", 360.0),
                face,
            },
            "along" => match self.path.look(c) {
                Some(Look::Str(data)) => match check_path_data(&data) {
                    Ok(()) => RepeatLayout::Along { path: data.into(), face },
                    Err(message) => {
                        c.err(at, "path", message, None);
                        RepeatLayout::Line
                    }
                },
                _ => {
                    c.err(at, "path", "`layout: @along` places the copies along `path: \"M…\"` (path data)".into(), None);
                    RepeatLayout::Line
                }
            },
            "line" => RepeatLayout::Line,
            other => {
                c.err(at, "layout", format!("`@{other}` is not a layout.{}", did_you_mean(other, crate::layers::REPEAT_LAYOUT)), Some("@grid, @radial, @along or @line"));
                RepeatLayout::Line
            }
        };
        let step = c.repeat_step(at, "step", self.step.look(c));
        let jitter = c.repeat_step(at, "jitter", self.jitter.look(c));
        let delay = self.delay.length(c, at, "delay").map_or(0.0, |d| d.value);
        Some(LayerKind::Repeat { layer: Box::new(template), items, count, layout, delay, step, jitter, seed: self.seed.konst(c, at, "seed", 1.0) as u32 })
    }
}

impl Reader<'_, '_> {
    /// A Repeat's `source`: a list (`in(@rows)` of a list input, or a list
    /// written in place), its length; None without one; one that is not a
    /// list is reported and makes no copies.
    fn repeat_source(&mut self, at: &At, value: Option<Look>) -> Option<usize> {
        let value = value?;
        let read = |c: &mut Self, name: &str| -> Option<usize> {
            match c.input(at, "source", name)? {
                InputValue::List(items) => Some(items.len()),
                other => {
                    c.err(at, "source", format!("`source` is a list, but input `{name}` is {}", other.describe()), Some("declare it as a list: `inputs: {rows: [\"a\", \"b\"]}` or `rows: input(@list)`"));
                    Some(0)
                }
            }
        };
        match value {
            Look::Arr(items) => Some(items.len()),
            Look::Obj(obj, Some("In")) => match self.get(obj, "name") {
                Some(Look::Id(name)) => read(self, &name),
                _ => Some(0),
            },
            // `input(@rows)` where `rows` is a declared input (not a type):
            // read as `in(@rows)`.
            Look::Obj(obj, Some("Input")) if matches!(self.get(obj, "ty"), Some(Look::Id(name)) if InputType::from_id(&name).is_none() && self.inputs.iter().any(|input| input.name == name)) => {
                let Some(Look::Id(name)) = self.get(obj, "ty") else { return Some(0) };
                read(self, &name)
            }
            other => {
                self.err(at, "source", format!("`source` is a list to make one copy per item of, not {}", other.kind()), Some("`source: in(@rows)` (a list input) or `source: [\"a\", \"b\", \"c\"]`"));
                Some(0)
            }
        }
    }

    /// A Repeat's `step` / `jitter`: the record `{x y rotate scale opacity}`.
    fn repeat_step(&mut self, at: &At, field: &str, value: Option<Look>) -> RepeatStep {
        match value {
            None => RepeatStep::default(),
            Some(Look::Obj(o, _)) => {
                let sat = self.at(o, at.field(field), at.loc);
                self.unknown_fields(&sat, &["x", "y", "rotate", "scale", "opacity"], field);
                let n = |c: &mut Self, name: &str| match c.get(o, name) {
                    None | Some(Look::Nil) => 0.0,
                    Some(value) => c.num_const(&sat, name, &value).unwrap_or(0.0),
                };
                RepeatStep { x: n(self, "x"), y: n(self, "y"), rotate: n(self, "rotate"), scale: n(self, "scale"), opacity: n(self, "opacity") }
            }
            Some(other) => {
                self.err(at, field, format!("`{field}` is `{{x y rotate scale opacity}}` per copy, not {}", other.kind()), None);
                RepeatStep::default()
            }
        }
    }
}

impl Freeze {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let time = match self.time.look(c) {
            // A moving time: a function or an animation of seconds.
            Some(value @ Look::Fn(_)) => FreezeTime::Moving(c.prop_of::<1>(at, "time", Some(value), [0.0])),
            Some(value @ Look::Obj(..)) if value.anim().is_some() => FreezeTime::Moving(c.prop_of::<1>(at, "time", Some(value), [0.0])),
            Some(_) => FreezeTime::Moment(self.time.time(c, at, "time").unwrap_or(Time::seconds(0.0))),
            None => {
                c.err(at, "time", "a Freeze shows the picture at its `time`".into(), Some("`Freeze{time: 16.2 size: vec2(640, 360)}`"));
                FreezeTime::Moment(Time::seconds(0.0))
            }
        };
        let frame = [c.width, c.height];
        let size = self.size.konst(c, at, "size", frame);
        LayerKind::Freeze { time, size }
    }
}

impl FlyThrough {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> LayerKind {
        let layers = match self.layers.look(c) {
            Some(Look::Arr(items)) => c.layers(at, "layers", &items),
            _ => {
                c.err(at, "layers", "a FlyThrough draws its `layers: […]` and flies into a letter of one of them".into(), None);
                Vec::new()
            }
        };
        // The named Text, or the first Text inside a layer with that
        // name (a Text with `window`/`animate` becomes a Group).
        fn first_text(layers: &[Layer]) -> Option<String> {
            layers.iter().find_map(|l| match &l.kind {
                LayerKind::Text { .. } => Some(l.path.clone()),
                LayerKind::Group { layers, .. } => first_text(layers),
                _ => None,
            })
        }
        fn find(layers: &[Layer], name: &str) -> Option<String> {
            layers.iter().find_map(|l| match (&l.name, &l.kind) {
                (Some(n), LayerKind::Text { .. }) if n == name => Some(l.path.clone()),
                (Some(n), LayerKind::Group { layers, .. }) if n == name => first_text(layers),
                (_, LayerKind::Group { layers, .. }) => find(layers, name),
                _ => None,
            })
        }
        let target = match self.into.look(c) {
            Some(Look::Str(name)) => match find(&layers, &name) {
                Some(path) => path,
                None => {
                    c.err(at, "into", format!("no Text named \"{name}\" among the FlyThrough's layers"), Some("`FlyThrough{into: \"title\" letter: 3 … layers: [Text{name: \"title\" …}]}`"));
                    String::new()
                }
            },
            _ => {
                c.err(at, "into", "`into` names the Text (by its `name`) to fly into".into(), None);
                String::new()
            }
        };
        let time = self.time.time(c, at, "time").unwrap_or(Time::seconds(0.0));
        LayerKind::FlyThrough {
            layers,
            target,
            letter: self.letter.konst(c, at, "letter", 0.0).max(0.0) as usize,
            time,
            duration: self.duration.konst(c, at, "duration", 0.6).max(0.01),
            zoom: self.zoom.konst(c, at, "zoom", 40.0).max(1.0),
        }
    }
}

impl Draw2D {
    pub(super) fn read(&self, c: &mut Reader, at: &At) -> Option<LayerKind> {
        let draw = match self.draw.look(c) {
            Some(Look::Fn(f)) => c.fn_prop(f, at, "draw"),
            _ => {
                c.err(at, "draw", "a Draw2D draws with `draw: fn(c, t, edit) { … }`".into(), None);
                return None;
            }
        };
        Some(LayerKind::Draw2d { draw, size: self.size.size(c, at, "size") })
    }
}

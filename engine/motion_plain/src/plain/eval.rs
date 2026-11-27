use super::*;
use crate::placement::{layer_clock, Placement};
use crate::sample::Sampler;
use crate::{Affine, Local};

impl PlainDoc {
    pub fn eval(&self, t: f64) -> PlainFrame {
        let mut errors = Vec::new();
        let mut sampler = Sampler { calls: None, errors: &mut errors, instance: 0, edit: t };
        let items = self.layers.iter().filter_map(|layer| layer.eval(&mut sampler, t, 0.0, &Affine::IDENTITY, true)).collect();
        PlainFrame(Frame { t, size: self.size, background: self.background, fps: self.fps, items, errors })
    }
}

impl Layer {
    fn eval(&self, sampler: &mut Sampler<'_>, t: f64, origin: f64, parent: &Affine, parent_static: bool) -> Option<Item> {
        let (lt, origin) = layer_clock(self.visible, self.span.as_ref(), t, origin)?;
        let per_unit = matches!(&self.kind, LayerKind::Text { props, .. } if props.by != TextBy::Whole);
        let Placement { world, local, opacity, blur, feather } =
            sampler.place(&self.transform, &self.opacity, &self.blur, &self.feather, lt, parent, per_unit);
        let is_static = parent_static && !self.animated;
        let kind = match &self.kind {
            LayerKind::Rect(p) => ItemKind::Primitive(p.eval(sampler, lt)),
            LayerKind::Ellipse(p) => ItemKind::Primitive(p.eval(sampler, lt)),
            LayerKind::Path(p) => ItemKind::Primitive(p.eval(sampler, lt)),
            LayerKind::Text { text, props } => {
                let size = sampler.sample(&props.size, lt, 0)[0].max(0.0);
                let units = sampler.text_units(text, props.by, &props.stagger, &self.transform, &self.opacity, &self.blur, &props.color, lt);
                let reveal = sampler.text_reveal(text, props.reveal.as_ref(), lt);
                ItemKind::Primitive(PrimitiveFrame::Text {
                    text: text.clone(), font: props.font.clone(), weight: props.weight, italic: props.italic,
                    axes: Vec::new(), size, align: props.align, valign: props.valign,
                    line_height: props.line_height, tracking: sampler.sample(&props.tracking, lt, 0)[0], wrap: props.wrap,
                    fit: props.fit, path: None, by: props.by, units, plate: None, reveal,
                    caret: None, outline: None,
                })
            }
            LayerKind::Group { layers, mask, props } => {
                let (size, placed) = props.placement(sampler, lt, &world, &local);
                let children = layers.iter().enumerate().filter_map(|(i, layer)| {
                    let delay = props.stagger.delay(i, layers.len());
                    layer.eval(sampler, lt - delay, origin + delay, &placed, is_static)
                }).collect();
                let mask = mask.as_ref().and_then(|m| m.eval(sampler, lt, origin, &placed, is_static)).map(Box::new);
                ItemKind::Group { children, mask, mask_mode: props.mask_mode, size }
            }
            LayerKind::Repeat { layer, props } => {
                let placed = world.then(&local.affine([0.0; 2]));
                let along = props.guide();
                let outer = sampler.instance;
                let mut children = Vec::with_capacity(props.count);
                for i in 0..props.count {
                    let (copy, opacity, delay) = props.place_copy(i, &placed, along.as_ref());
                    sampler.instance = outer + i;
                    if let Some(mut item) = layer.eval(sampler, lt - delay, origin + delay, &copy, is_static) {
                        item.opacity *= opacity;
                        item.suffix_paths(i);
                        children.push(item);
                    }
                }
                sampler.instance = outer;
                ItemKind::Group { children, mask: None, mask_mode: MaskMode::default(), size: None }
            }
        };
        Some(Item { path: self.path.clone(), world, local, opacity, blur, feather, feather_side: self.feather_side,
            effects: Vec::new(), blend: self.blend, is_static, kind })
    }
}

impl Item {
    fn suffix_paths(&mut self, index: usize) {
        self.path = format!("{}#{index}", self.path);
        if let ItemKind::Group { children, mask, .. } = &mut self.kind {
            for child in children { child.suffix_paths(index); }
            if let Some(mask) = mask { mask.suffix_paths(index); }
        }
    }
}

impl RectProps {
    pub fn eval(&self, sampler: &mut Sampler<'_>, t: f64) -> PrimitiveFrame {
        let size = sampler.sample(&self.size, t, 0);
        let radius = sampler.sample(&self.radius, t, 0)[0].max(0.0);
        let (fill, stroke, width, trim) = sampler.paint(&self.fill, &self.stroke, &self.width, &self.trim_start, &self.trim_end, t);
        PrimitiveFrame::Rect { size, radius,
            fill, stroke, width, trim, cap: self.cap }
    }
}

impl EllipseProps {
    pub fn eval(&self, sampler: &mut Sampler<'_>, t: f64) -> PrimitiveFrame {
        let size = sampler.sample(&self.size, t, 0);
        let (fill, stroke, width, trim) = sampler.paint(&self.fill, &self.stroke, &self.width, &self.trim_start, &self.trim_end, t);
        PrimitiveFrame::Ellipse { size, fill, stroke, width, trim, cap: self.cap }
    }
}

impl PathProps {
    pub fn eval(&self, sampler: &mut Sampler<'_>, t: f64) -> PrimitiveFrame {
        let (fill, stroke, width, trim) = sampler.paint(&self.fill, &self.stroke, &self.width, &self.trim_start, &self.trim_end, t);
        PrimitiveFrame::Path { path: self.data.at(t), fill, stroke, width, trim, cap: self.cap, join: self.join }
    }
}

impl GroupProps {
    pub fn placement(&self, sampler: &mut Sampler<'_>, t: f64, world: &Affine, local: &Local) -> (Option<[f64; 2]>, Affine) {
        let size = self.size.as_ref().map(|s| sampler.sample(s, t, 0));
        (size, world.then(&local.affine(size.unwrap_or([0.0; 2]))))
    }

}

impl Sampler<'_> {
    fn paint(&mut self, fill: &Option<Prop<4>>, stroke: &Option<Prop<4>>, width: &Prop<1>,
                 start: &Prop<1>, end: &Prop<1>, t: f64) -> (Option<Color>, Option<Color>, f64, [f64; 2]) {
        (self.opt4(fill, t), self.opt4(stroke, t), self.sample(width, t, 0)[0].max(0.0), self.trim(start, end, t))
    }
}

use crate::plain::{EllipseProps, GroupProps, PathProps, RectProps, RepeatProps, TextProps};
use crate::{Layer, LayerKind, MotionDoc, PathShape, Prop};

/// Coarse samples on the output frame grid, plus resolved spans and keys
/// and the frames on either side. Does not allocate the full frame grid.
/// Arbitrary property functions are sampled, not solved analytically.
pub fn sample_times(doc: &MotionDoc, sample_fps: f64) -> Vec<f64> {
    let fps = if sample_fps.is_finite() && sample_fps > 0.0 { sample_fps } else { 10.0 };
    let stride = (doc.fps / fps).round().max(1.0) as usize;
    let count = (doc.duration * doc.fps - 1e-9).ceil().max(1.0) as usize;
    let mut times: Vec<f64> = (0..count).step_by(stride).map(|i| i as f64 / doc.fps).collect();
    times.push((count - 1) as f64 / doc.fps);
    let mut keys = Vec::new();
    for layer in &doc.layers { layer_times(layer, 0.0, doc.duration, &mut keys); }
    for t in keys {
        // Nested local clocks can round a sum just below their boundary.
        // Prefer the output frame there, otherwise the boundary's right
        // neighbour, so a half-open span is sampled on the intended side.
        let grid = (t * doc.fps).round() / doc.fps;
        times.push(if (grid - t).abs() < 1e-9 { grid } else { t.next_up() });
        times.push(((t * doc.fps).ceil() - 1.0) / doc.fps);
        times.push((t * doc.fps).ceil() / doc.fps);
    }
    times.retain(|t| t.is_finite() && *t >= 0.0 && *t < doc.duration);
    times.sort_by(f64::total_cmp);
    times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    times
}

fn prop_times<const N: usize>(p: &Prop<N>, start: f64, end: f64, out: &mut Vec<f64>) {
    let Prop::Track(track) = p else { return };
    for key in &track.keys { out.push(start + key.t.value); }
    if let Some(repeat) = &track.repeat {
        if repeat.period.value > 0.0 {
            let count = ((end - start) / repeat.period.value).ceil().max(0.0) as usize;
            let count = repeat.count.map_or(count, |n| count.min(n as usize));
            for i in 1..count {
                for key in &track.keys { out.push(start + i as f64 * repeat.period.value + key.t.value); }
            }
        }
    }
}

fn layer_times(layer: &Layer, origin: f64, end: f64, out: &mut Vec<f64>) {
    let (start, end) = layer.span.as_ref().map_or((origin, end), |s| (origin + s.start.value, (origin + s.start.value + s.length.value).min(end)));
    out.extend([start, end]);
    let tr = &layer.transform;
    macro_rules! props { ($($p:expr),* $(,)?) => { $(prop_times($p, start, end, out);)* }; }
    props!(&tr.at, &tr.x, &tr.y, &tr.scale, &tr.rotate, &layer.opacity, &layer.blur);
    if let Some(along) = &tr.along { props!(&along.progress); }
    match &layer.kind {
        LayerKind::Text { props: TextProps { size, color, tracking, reveal, by, stagger, .. }, axes, path, source, .. } => {
            props!(size, color, tracking);
            if let Some(reveal) = reveal { props!(reveal); }
            for (_, p) in axes { props!(p); }
            if let Some((_, p)) = path { props!(p); }
            if let crate::TextSource::Text(text) = source {
                let n = crate::unit_ranges(text, *by).len();
                for i in 1..n {
                    let shifted = start + stagger.delay(i, n);
                    prop_times(&tr.x, shifted, end, out);
                    prop_times(&tr.y, shifted, end, out);
                    prop_times(&tr.scale, shifted, end, out);
                    prop_times(&tr.rotate, shifted, end, out);
                    prop_times(&layer.opacity, shifted, end, out);
                }
            }
        }
        LayerKind::Rect(RectProps { size, fill, stroke, width, trim_start, trim_end, .. }) | LayerKind::Ellipse(EllipseProps { size, fill, stroke, width, trim_start, trim_end, .. }) => {
            props!(size, width, trim_start, trim_end);
            for p in [fill, stroke].into_iter().flatten() { props!(p); }
        }
        LayerKind::Path(PathProps { data, fill, stroke, width, trim_start, trim_end, .. }) => {
            props!(width, trim_start, trim_end);
            for p in [fill, stroke].into_iter().flatten() { props!(p); }
            if let PathShape::Morph { track, .. } = data { for key in &track.keys { out.push(start + key.t.value); } }
        }
        LayerKind::Group { layers, mask, props: GroupProps { stagger, size, .. }, .. } => {
            if let Some(size) = size { props!(size); }
            for (i, child) in layers.iter().enumerate() { layer_times(child, start + stagger.delay(i, layers.len()), end, out); }
            if let Some(mask) = mask { layer_times(mask, start, end, out); }
        }
        LayerKind::Repeat { layer, items, props: RepeatProps { count, delay, .. }, .. } => {
            for i in 0..*count { layer_times(items.get(i).unwrap_or(layer), start + i as f64 * delay, end, out); }
        }
        LayerKind::FlyThrough { layers, time, duration, .. } => {
            out.extend([start + time.value, start + time.value + duration]);
            for child in layers { layer_times(child, start, end, out); }
        }
        LayerKind::Anchored { layer, .. } => layer_times(layer, start, end, out),
        LayerKind::Image { size, .. } | LayerKind::Video { size, .. } | LayerKind::Svg { size, .. }
        | LayerKind::Shader { size, .. } | LayerKind::Scene3D { size, .. } | LayerKind::Scene3dDoc { size, .. }
        | LayerKind::Vector { size, .. } | LayerKind::Draw2d { size, .. } => { if let Some(size) = size { props!(size); } }
        LayerKind::Widget { size, .. } => { props!(size); }
        _ => {}
    }
}

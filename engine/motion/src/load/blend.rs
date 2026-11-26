//! One loader for value and pose blend spaces. The axes and grid are
//! validated once; frame evaluation only visits the surrounding values.

use super::{At, Reader};
use crate::model::{BlendForm, Prop};
use crate::obj::Look;
use crate::value::fmt_num;

impl Reader<'_, '_> {
    /// `read` gives the meaning of each value: keyable numbers/vectors/
    /// colours, or a named pose. Both use the same shape and kind checks.
    pub(super) fn blend_form<T>(&mut self, at: &At, field: &str, value: &Look, mut read: impl FnMut(&mut Self, &At, &str, &Look) -> Option<T>) -> Option<BlendForm<T>> {
        let obj = value.obj()?;
        let x = self.get(obj, "x").unwrap_or(Look::Nil);
        let x = self.blend_control(at, &format!("{field}.x"), &x)?;
        let ease = self.get(obj, "ease").unwrap_or(Look::Nil);
        let ease = self.ease(at, &format!("{field}.ease"), &ease);
        let (cols, rows, y, raw) = if value.is("Blend") {
            let stops = self.get(obj, "stops").unwrap_or(Look::Nil);
            let (cols, raw) = self.blend_stops(at, field, stops)?;
            (cols, vec![0.0], None, raw)
        } else {
            let y = self.get(obj, "y").unwrap_or(Look::Nil);
            let y = self.blend_control(at, &format!("{field}.y"), &y)?;
            let Some(Look::Obj(grid, None)) = self.get(obj, "grid") else {
                self.err(at, field, format!("`{field}`: blend2 grid is {{cols: [...] rows: [...] values: [[...], ...]}}"), None);
                return None;
            };
            let cols = self.blend_axis(at, &format!("{field}.cols"), self.get(grid, "cols"))?;
            let rows = self.blend_axis(at, &format!("{field}.rows"), self.get(grid, "rows"))?;
            let Some(Look::Arr(values)) = self.get(grid, "values") else {
                self.err(at, field, format!("`{field}`: blend2 grid `values` is an array of rows"), None);
                return None;
            };
            if values.len() != rows.len() {
                self.err(at, field, format!("`{field}`: ragged blend2 grid: values has {} rows, expected {}", values.len(), rows.len()), None);
                return None;
            }
            let mut raw = Vec::new();
            for (r, row) in values.into_iter().enumerate() {
                match row {
                    Look::Arr(values) if values.len() == cols.len() => raw.extend(values),
                    other => {
                        let got = match other { Look::Arr(values) => format!("{} values", values.len()), other => other.kind() };
                        self.err(at, field, format!("`{field}`: ragged blend2 grid: row {r} has {got}, expected {} values (one per column)", cols.len()), None);
                        return None;
                    }
                }
            }
            (cols, rows, Some(y), raw)
        };
        let mut kind = None;
        let mut values = Vec::with_capacity(raw.len());
        for (i, value) in raw.iter().enumerate() {
            let path = if y.is_some() { format!("{field}.values[{}][{}]", i / cols.len(), i % cols.len()) } else { format!("{field}.values[{i}]") };
            self.blend_kind(at, &path, value, &mut kind)?;
            values.push(read(self, at, &path, value)?);
        }
        Some(BlendForm { x, y, cols, rows, values, value_kind: kind, ease })
    }

    fn blend_control(&mut self, at: &At, field: &str, value: &Look) -> Option<Prop<1>> {
        if matches!(value, Look::Bool(_)) {
            self.err(at, field, format!("`{field}` is a keyable number, not a bool"), None);
            return None;
        }
        self.blend_kind(at, field, value, &mut Some(1))?;
        self.prop_value::<1>(at, field, value)
    }

    /// A representative value for fields whose kind is inferred (effects,
    /// widget properties). Shape errors are reported by `blend_form`.
    pub(super) fn blend_first(&self, value: &Look) -> Option<Look> {
        let obj = value.obj()?;
        if value.is("Blend2") {
            let grid = self.get(obj, "grid")?.obj()?;
            let Look::Arr(rows) = self.get(grid, "values")? else { return None };
            return rows.into_iter().filter_map(|row| match row { Look::Arr(values) => Some(values), _ => None })
                .flatten().find_map(|value| self.blend_sample(value));
        }
        match self.get(obj, "stops")? {
            Look::Arr(values) => values.into_iter().find_map(|value| self.blend_sample(value)),
            Look::Obj(obj, None) => {
                let Look::Arr(values) = self.get(obj, "values")? else { return None };
                values.into_iter().find_map(|value| self.blend_sample(value))
            }
            _ => None,
        }
    }

    fn blend_sample(&self, value: Look) -> Option<Look> {
        if matches!(value, Look::Fn(_)) { return None; }
        if value.anim().is_some() { return self.blend_sample(self.anim_sample(&value)?); }
        Some(value)
    }

    fn blend_axis(&mut self, at: &At, field: &str, value: Option<Look>) -> Option<Vec<f64>> {
        let Some(Look::Arr(values)) = value else {
            self.err(at, field, format!("`{field}` is a nonempty list of constant numeric stops"), None);
            return None;
        };
        let mut stops = Vec::with_capacity(values.len());
        for value in values {
            let Some(n) = value.num() else {
                self.err(at, field, format!("`{field}` stops must be finite constant numbers, not {}", value.kind()), None);
                return None;
            };
            stops.push(n);
        }
        self.check_blend_stops(at, field, &stops)?;
        Some(stops)
    }

    fn check_blend_stops(&mut self, at: &At, field: &str, stops: &[f64]) -> Option<()> {
        if stops.is_empty() {
            self.err(at, field, format!("`{field}`: empty blend stop list; give at least one value"), None);
            return None;
        }
        if let Some(pair) = stops.windows(2).find(|p| p[1] <= p[0]) {
            self.err(at, field, format!("`{field}` blend stops must be strictly increasing: {} follows {}", fmt_num(pair[1]), fmt_num(pair[0])), None);
            return None;
        }
        Some(())
    }

    fn blend_stops(&mut self, at: &At, field: &str, value: Look) -> Option<(Vec<f64>, Vec<Look>)> {
        match value {
            Look::Arr(values) => {
                let last = values.len().saturating_sub(1).max(1) as f64;
                let stops = (0..values.len()).map(|i| i as f64 / last).collect::<Vec<_>>();
                self.check_blend_stops(at, field, &stops)?;
                Some((stops, values))
            }
            Look::Obj(obj, None) => {
                let stops = self.blend_axis(at, &format!("{field}.at"), self.get(obj, "at"))?;
                let Some(Look::Arr(values)) = self.get(obj, "values") else {
                    self.err(at, &format!("{field}.values"), format!("`{field}.values` is a nonempty array with one value per `at` stop"), None);
                    return None;
                };
                if stops.len() != values.len() {
                    self.err(at, field, format!("`{field}` blend `at` and `values` lengths differ: {} stops, {} values; give one value per stop", stops.len(), values.len()), None);
                    return None;
                }
                Some((stops, values))
            }
            other => {
                self.err(at, field, format!("`{field}` blend stops are [a, b] or {{at: [0, 1] values: [a, b]}}, not {}", other.kind()), None);
                None
            }
        }
    }

    /// Check kinds before scalar-to-vec2 field shorthand could conceal a
    /// mismatch. Unknown function results are checked by the field sampler.
    fn blend_kind(&mut self, at: &At, field: &str, value: &Look, expected: &mut Option<usize>) -> Option<()> {
        let kind = match value {
            Look::Fn(_) => return Some(()),
            Look::Str(_) if self.color_of(value).is_some() => 4,
            Look::Id(_) | Look::Str(_) => 0, // named poses
            Look::Num(_) => 1,
            Look::Arr(values) => values.len(),
            Look::Color(_) => 4,
            Look::Obj(obj, _) if value.anim().is_some() => {
                let values = match value.anim() {
                    Some("Tween") => vec![self.get(*obj, "from")?, self.get(*obj, "to")?],
                    Some("Loop") => vec![self.get(*obj, "inner")?],
                    Some("Keys" | "Sequence") => {
                        let keys = value.is("Keys");
                        let Some(Look::Arr(points)) = self.get(*obj, if keys { "points" } else { "steps" }) else { return Some(()) };
                        points.into_iter().filter_map(|p| match p { Look::Arr(parts) => parts.get(if keys { 1 } else { 0 }).cloned(), _ => None }).collect()
                    }
                    Some("Blend") => self.blend_stops(at, field, self.get(*obj, "stops").unwrap_or(Look::Nil))?.1,
                    Some("Blend2") => {
                        let Some(grid) = self.get(*obj, "grid").and_then(|v| v.obj()) else { return Some(()) };
                        let Some(Look::Arr(rows)) = self.get(grid, "values") else { return Some(()) };
                        rows.into_iter().flat_map(|r| match r { Look::Arr(v) => v, _ => Vec::new() }).collect()
                    }
                    _ => return Some(()),
                };
                for value in values { self.blend_kind(at, field, &value, expected)?; }
                return Some(());
            }
            Look::Obj(obj, _) if self.get(*obj, "color").is_some() => 4,
            _ => return Some(()), // the typed read explains unsupported values
        };
        if expected.is_some_and(|previous| previous != kind) {
            self.err(at, field, format!("`{field}`: mismatched blend value kinds; every value must have the same kind (number, vec2, vec3, colour or named pose)"), None);
            return None;
        }
        *expected = Some(kind);
        Some(())
    }
}

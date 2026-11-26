//! Motion as the document the editor panels edit: its named paths, outline,
//! palette, keyed values and the one edit primitive, behind the open
//! `makepad-editor-panel` trait, and its values ([`MotionValues`], the
//! Motion document's `ValueSource`).

use makepad_edit_values::{Live, Splice, Value, ValueKind, ValueSite, ValueSource};
use makepad_editor_panel as panel;
use makepad_motion::edit::{self, Value as MotionValue};
use makepad_motion::paths::{self, docs, File as MotionFile};
use makepad_motion::schema::code::{self, CodeOutline, RegionKind};
use makepad_motion::{lane, palette};
use std::any::Any;
use std::ops::Range;

/// The Motion document kind: install it with `makepad_editor_panel::register`.
pub struct MotionDoc;

fn outline_of(code: &dyn Any) -> &CodeOutline {
    code.downcast_ref::<CodeOutline>().expect("a Motion document's outline is a CodeOutline")
}

fn table_of(table: &dyn Any) -> &docs::DocTable {
    table.downcast_ref::<docs::DocTable>().expect("a Motion document set's table is a DocTable")
}

fn files<'a>(files: &[panel::File<'a>]) -> Vec<MotionFile<'a>> {
    files.iter().map(|f| MotionFile { name: f.name, text: f.text, code: outline_of(f.code) }).collect()
}

/// Motion's files as the panel reads them (the host's own `paths::File`s).
pub fn panel_files<'a>(files: &[MotionFile<'a>]) -> Vec<panel::File<'a>> {
    files.iter().map(|f| panel::File { name: f.name, text: f.text, code: f.code }).collect()
}

/// A path's site as a value site: its id, kind, bytes and value.
pub fn value_site(path: &str, site: paths::Site) -> ValueSite {
    let value = match site.kind {
        k if k.is_numeric() => site.text.trim().trim_end_matches(|c: char| c.is_ascii_alphabetic()).parse::<f64>().map_or_else(|_| Value::Code(site.text.clone()), Value::Num),
        ValueKind::Bool => Value::Bool(site.text.trim() == "true"),
        _ => Value::Code(site.text.clone()),
    };
    ValueSite { id: path.to_string(), kind: site.kind, span: Some((site.file, site.range)), text: site.text, value, live: Live::Patch, ..Default::default() }
}

fn site_out(site: &ValueSite) -> paths::Site {
    paths::Site { file: site.file(), range: site.range(), kind: site.kind, text: site.text.clone() }
}

fn value_out(value: &Value) -> MotionValue {
    match value {
        Value::Num(n) => MotionValue::Number(*n),
        Value::Text(t) => MotionValue::Text(t.clone()),
        Value::Bool(b) => MotionValue::Bool(*b),
        Value::Colour(c) => MotionValue::Colour(c.map(|x| x as f64)),
        Value::Code(c) => MotionValue::Code(c.clone()),
        Value::Choice(i) => MotionValue::Code(i.to_string()),
        Value::Point(v) => MotionValue::Code(format!("vec{}({})", v.len(), v.iter().map(|x| makepad_motion::patch::fmt_num(*x)).collect::<Vec<_>>().join(", "))),
        Value::Profile(p) => MotionValue::Code(format!("[{}]", p.iter().map(|[x, y]| format!("[{}, {}]", makepad_motion::patch::fmt_num(*x), makepad_motion::patch::fmt_num(*y))).collect::<Vec<_>>().join(", "))),
    }
}

/// A Motion document's values over its files (the document and the modules
/// it imports): Motion's named paths.
pub struct MotionValues<'a> {
    files: Vec<MotionFile<'a>>,
}

impl<'a> MotionValues<'a> {
    pub fn new(files: Vec<MotionFile<'a>>) -> MotionValues<'a> {
        MotionValues { files }
    }
}

impl ValueSource for MotionValues<'_> {
    fn sites(&self) -> Vec<ValueSite> {
        paths::list(&self.files)
            .into_iter()
            .map(|e| ValueSite { id: e.path, kind: e.kind, text: e.value, label: e.label.unwrap_or_default(), live: Live::Patch, ..Default::default() })
            .collect()
    }

    fn resolve(&self, id: &str) -> Result<ValueSite, String> {
        paths::resolve_exact(&self.files, id).map(|site| value_site(id, site))
    }

    fn write(&self, id: &str, value: &Value) -> Result<Vec<Splice>, String> {
        let w = edit::write(&self.files, id, &value_out(value))?;
        Ok(vec![Splice { file: w.file, range: w.range, text: w.literal }])
    }

    fn nearest(&self, id: &str) -> Vec<String> {
        paths::nearest(&self.files, id)
    }
}

impl panel::PanelDoc for MotionDoc {
    fn outline(&self, text: &str) -> Box<dyn Any> {
        Box::new(code::outline(text))
    }

    fn let_region(&self, _text: &str, code: &dyn Any, name: &str) -> Option<Range<usize>> {
        outline_of(code).regions.iter().find(|r| r.kind == RegionKind::Let && r.depth == 0 && r.name == name).map(|r| r.range.clone())
    }

    fn let_of_literal(&self, text: &str, code: &dyn Any, site: &Range<usize>) -> Option<String> {
        let region = outline_of(code).regions.iter().find(|r| r.kind == RegionKind::Let && r.depth == 0 && r.range.start <= site.start && site.end <= r.range.end)?;
        let head = &text[region.range.start..site.start];
        let before = head[head.find('=')? + 1..].trim();
        let tag_only = before.is_empty() || (before.starts_with("/**") && before.ends_with("*/") && before.matches("*/").count() == 1);
        let after = text[site.end..region.range.end].trim();
        let after = after.split("//").next().unwrap_or("").trim();
        (tag_only && after.is_empty()).then(|| region.name.clone())
    }

    fn fns_like(&self, files_in: &[panel::File], path: &str) -> Vec<String> {
        let files = files(files_in);
        let Ok(site) = paths::resolve(&files, path) else { return Vec::new() };
        let name = site.text.trim();
        let params_of = |qualified: &str| -> Option<Vec<String>> {
            match qualified.split_once('.') {
                Some((module, f)) => files.iter().find(|x| x.name == module)?.code.fns.iter().find(|(n, _)| n == f).map(|(_, p)| p.clone()),
                None => files.iter().find(|x| x.name.is_empty())?.code.fns.iter().find(|(n, _)| n == qualified).map(|(_, p)| p.clone()),
            }
        };
        let Some(params) = params_of(name) else { return Vec::new() };
        let mut out = Vec::new();
        for f in &files {
            let prefix = if f.name.is_empty() { String::new() } else { format!("{}.", f.name) };
            for (n, p) in &f.code.fns {
                if *p == params && !n.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_') {
                    out.push(format!("{prefix}{n}"));
                }
            }
        }
        out
    }

    fn values<'a>(&'a self, files_in: &'a [panel::File<'a>]) -> Box<dyn ValueSource + 'a> {
        Box::new(MotionValues::new(files(files_in)))
    }

    fn resolve_in(&self, files_in: &[panel::File], table: &dyn Any, path: &str) -> Result<Vec<panel::Bound>, String> {
        let files = files(files_in);
        docs::resolve(&files, table_of(table), path).map(|bounds| bounds.into_iter().map(|b| panel::Bound { site: value_site(path, b.site), scale: b.scale, member: b.member }).collect())
    }

    fn list_in(&self, files_in: &[panel::File], table: &dyn Any) -> Vec<(ValueSite, usize)> {
        let files = files(files_in);
        docs::list(&files, table_of(table)).into_iter().map(|e| (ValueSite { id: e.path, kind: e.kind, text: e.value, label: e.label.unwrap_or_default(), live: Live::Patch, ..Default::default() }, e.reach)).collect()
    }

    fn snap(&self, table: &dyn Any, path: &str, from: f64, to: f64) -> f64 {
        docs::snap(table_of(table), path, from, to)
    }

    fn snaps(&self, path: &str) -> bool {
        docs::snaps(path)
    }

    fn palette(&self, text: &str) -> Vec<panel::Swatch> {
        palette::of(text).into_iter().map(|w| panel::Swatch { rgba: w.rgba, name: w.name, sites: w.sites }).collect()
    }

    fn palette_sites(&self, text: &str) -> Vec<(Range<usize>, [f64; 4])> {
        palette::sites(text)
    }

    fn recolour(&self, text: &str, map: &[([f64; 4], [f64; 4])]) -> String {
        palette::set(text, map)
    }

    fn recolour_at(&self, text: &str, sites: &[(Range<usize>, [f64; 4])], map: &[([f64; 4], [f64; 4])]) -> String {
        palette::set_at(text, sites, map)
    }

    fn keys_sample(&self, text: &str, range: Range<usize>, at: f64) -> Option<f64> {
        lane::sample(&lane::parse(text, range)?, at)
    }

    fn keys_set(&self, text: &str, range: Range<usize>, at: f64, half: f64, value: f64) -> Option<(Range<usize>, String)> {
        let keys = lane::parse(text, range)?;
        let v = makepad_motion::patch::fmt_num(value);
        Some(match keys.keys.iter().find(|k| k.t.is_some_and(|kt| (kt - at).abs() < half)) {
            Some(k) => (k.v_range.clone(), v),
            None => lane::add_key(&keys, at, v.parse().unwrap_or(0.0)),
        })
    }

    fn place(&self, files_in: &[panel::File], path: &str, value: &Value) -> Result<(Range<usize>, String), String> {
        let (_, range, literal) = edit::place(&files(files_in), path, &value_out(value))?;
        Ok((range, literal))
    }

    fn literal_token(&self, text: &str, range: &Range<usize>) -> Option<Range<usize>> {
        edit::literal_token(text, range)
    }

    fn form(&self, site: &ValueSite, value: &Value) -> Result<String, String> {
        edit::form(&site_out(site), &value_out(value))
    }
}

/// The Motion document kind, to install ([`makepad_editor_panel::register`]).
pub fn motion_doc() -> std::rc::Rc<dyn panel::PanelDoc> {
    std::rc::Rc::new(MotionDoc)
}

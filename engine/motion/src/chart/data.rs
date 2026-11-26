//! Chart data: inline lists, CSV or JSON text, one or more series, and a
//! long table split into datasets (`split: "year"` for a bar race).

use super::build::*;
use crate::obj;
use makepad_script::*;

/// One datum (a bar, a point, a slice, a node, a card, a milestone).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Datum {
    pub label: String,
    /// The first series' value (the only one for most charts).
    pub value: f64,
    /// One value per series (at least one).
    pub values: Vec<f64>,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub r: Option<f64>,
    pub group: Option<String>,
    /// A timeline event's text, a card's caption.
    pub text: Option<String>,
    /// A KPI's change (0.12 = +12%).
    pub delta: Option<f64>,
    /// A KPI's sparkline.
    pub trend: Vec<f64>,
    /// A choropleth region (`europe`, `asia`, …).
    pub region: Option<String>,
    /// A KPI card's own number format: `(format, prefix, suffix)`.
    pub format: Option<(String, Option<String>, Option<String>)>,
    /// A Gantt task's span (a milestone: `end` equal to `start`, or none).
    pub start: Option<f64>,
    pub end: Option<f64>,
    /// A Gantt task's review status (`done`, `late`, `risk`, `planned`).
    pub status: Option<String>,
    /// A probability row: the token sampled at this step.
    pub pick: bool,
}

impl Datum {
    fn with_value(label: String, value: f64) -> Datum {
        Datum { label, value, values: vec![value], ..Default::default() }
    }
}

/// One dataset: its rows and, for several value columns, the series names.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Table {
    pub series: Vec<String>,
    pub rows: Vec<Datum>,
    /// The `split` column's value this dataset was cut at ("2021").
    pub key: Option<String>,
}

impl Table {
    pub fn series_count(&self) -> usize {
        self.rows.iter().map(|d| d.values.len()).max().unwrap_or(1).max(self.series.len()).max(1)
    }
}

/// Datasets from a `data` list, or CSV / JSON text; `split` cuts a long
/// table into one dataset per value of that column.
pub(crate) fn tables_of(b: &B, value: ScriptValue, split: Option<&str>) -> Result<Vec<Table>, String> {
    if let Some(items) = items_of(b, value) {
        let rows: Vec<Row> = items.into_iter().enumerate().map(|(i, item)| row_of(b, item, i)).collect::<Result<_, _>>()?;
        return Ok(split_rows(rows, split));
    }
    match b.with(|vm| obj::string(vm, value)) {
        Some(text) => tables_of_text(b, &text, split),
        None => Err(format!("chart data is a list, CSV or JSON text, not {}", kind_of(b, value))),
    }
}

/// Datasets from CSV or JSON text.
pub(crate) fn tables_of_text(b: &B, text: &str, split: Option<&str>) -> Result<Vec<Table>, String> {
    let trimmed = text.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        Ok(split_rows(json_rows(b, trimmed)?, split))
    } else {
        csv_tables(trimmed, split)
    }
}

/// A row before it is split: the datum and its fields by name (for `split`).
struct Row {
    datum: Datum,
    fields: Vec<(String, String)>,
}

fn split_rows(rows: Vec<Row>, split: Option<&str>) -> Vec<Table> {
    let Some(split) = split else {
        return vec![Table { series: Vec::new(), rows: rows.into_iter().map(|r| r.datum).collect(), key: None }];
    };
    let mut tables: Vec<Table> = Vec::new();
    for row in rows {
        let key = row.fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(split)).map(|(_, v)| v.clone()).unwrap_or_default();
        match tables.iter_mut().find(|t| t.key.as_deref() == Some(key.as_str())) {
            Some(table) => table.rows.push(row.datum),
            None => tables.push(Table { series: Vec::new(), rows: vec![row.datum], key: Some(key) }),
        }
    }
    tables
}

fn cell_text(b: &B, v: ScriptValue) -> Option<String> {
    if let Some(t) = text_of(b, v) {
        return Some(t);
    }
    let n = v.as_number()?;
    Some(crate::format_number(n, if n.fract() == 0.0 { 0 } else { 2 }, None))
}

fn row_of(b: &B, item: ScriptValue, i: usize) -> Result<Row, String> {
    let datum = if let Some(n) = item.as_number() {
        Datum::with_value(format!("{}", i + 1), n)
    } else if let Some(label) = text_of(b, item) {
        Datum::with_value(label, 0.0)
    } else if let Some(parts) = items_of(b, item).filter(|p| !p.is_empty()) {
        if let Some(l) = text_of(b, parts[0]) {
            let values: Vec<f64> = parts[1..].iter().filter_map(|p| number_of(b, *p)).collect();
            let text = parts[1..].iter().find_map(|p| b.with(|vm| obj::string(vm, *p)));
            Datum { label: l, value: values.first().copied().unwrap_or(0.0), values: if values.is_empty() { vec![0.0] } else { values }, text, ..Default::default() }
        } else if let (Some(x), true) = (parts[0].as_number(), parts.len() >= 2) {
            let y = number_of(b, parts[1]).ok_or_else(|| format!("datum {i} is `[x, y]` or `[x, y, r]`"))?;
            Datum { value: y, values: vec![y], x: Some(x), y: Some(y), r: parts.get(2).and_then(|r| number_of(b, *r)), ..Default::default() }
        } else {
            return Err(format!("datum {i} is `[\"label\", value, …]`, `[\"date\", \"text\"]` or `[x, y]`"));
        }
    } else if let Some(o) = item.as_object().filter(|_| !b.with(|vm| obj::is_fn(vm, item))) {
        return Ok(obj_row(b, o));
    } else {
        return Err(format!("datum {i} is {}", kind_of(b, item)));
    };
    Ok(Row { datum, fields: Vec::new() })
}

fn obj_row(b: &B, o: ScriptObject) -> Row {
    let fields: Vec<(String, ScriptValue, ScriptObject)> = b.with(|vm| obj::fields(vm, o));
    let get = |k: &str| fields.iter().find(|(f, _, _)| f == k).map(|(_, v, _)| *v);
    let text = |keys: &[&str]| keys.iter().find_map(|k| get(k).and_then(|v| cell_text(b, v)));
    let number = |keys: &[&str]| keys.iter().find_map(|k| get(k).and_then(|v| number_of(b, v)));
    let list = |key: &str| match get(key).and_then(|v| items_of(b, v)) {
        Some(items) => items.into_iter().filter_map(|v| number_of(b, v)).collect::<Vec<f64>>(),
        None => Vec::new(),
    };
    let values = list("values");
    let value = number(&["value", "y", "count", "amount"]).or_else(|| values.first().copied()).unwrap_or(0.0);
    let datum = Datum {
        label: text(&["label", "name", "id", "date", "year"]).unwrap_or_default(),
        value,
        values: if values.is_empty() { vec![value] } else { values },
        x: number(&["x", "lon", "lng", "longitude"]),
        y: number(&["y", "lat", "latitude"]),
        r: number(&["r", "size"]),
        group: text(&["group", "category"]),
        text: text(&["text", "note", "event", "caption", "description"]),
        delta: number(&["delta", "change"]),
        trend: list("trend"),
        region: text(&["region", "continent"]),
        format: match (text(&["format"]), text(&["prefix"]), text(&["suffix"])) {
            (None, None, None) => None,
            (f, p, s) => Some((f.unwrap_or_else(|| "plain".into()), p, s)),
        },
        start: number(&["start", "from", "begin"]),
        end: number(&["end", "to", "due", "finish"]),
        status: text(&["status", "state"]),
        pick: get("pick").and_then(|v| v.as_bool()) == Some(true),
    };
    let fields = fields.iter().filter_map(|(k, v, _)| Some((k.clone(), cell_text(b, *v)?))).collect();
    Row { datum, fields }
}

fn json_rows(b: &B, text: &str) -> Result<Vec<Row>, String> {
    let value: makepad_strict_json::Value = makepad_strict_json::parse(text.as_bytes()).map_err(|e| format!("chart JSON: {e}"))?;
    let value = json_value(b, &value);
    let items = match items_of(b, value) {
        Some(items) => items,
        None => match field_of(b, value, "data").and_then(|d| items_of(b, d)) {
            Some(items) if value.as_object().is_some() => items,
            _ if value.as_object().is_some() => return Err("chart JSON is a list, or an object with `data: […]`".into()),
            _ => return Err("chart JSON is a list".into()),
        },
    };
    items.into_iter().enumerate().map(|(i, item)| row_of(b, item, i)).collect()
}

/// A JSON value as the values a document writes.
fn json_value(b: &B, v: &makepad_strict_json::Value) -> ScriptValue {
    use makepad_strict_json::Value as J;
    match v {
        J::Null => NIL,
        J::Bool(x) => (*x).into(),
        J::Int(n) => (*n as f64).into(),
        J::F64(n) => (*n).into(),
        J::Str(t) => s(b, t),
        J::Arr(items) => {
            let values: Vec<ScriptValue> = items.iter().map(|x| json_value(b, x)).collect();
            list(b, values)
        }
        J::Obj(fields) => {
            let values: Vec<(String, ScriptValue)> = fields.iter().map(|(k, x)| (k.clone(), json_value(b, x))).collect();
            let refs: Vec<(&str, ScriptValue)> = values.iter().map(|(k, v)| (k.as_str(), *v)).collect();
            obj(b, None, refs)
        }
    }
}

/// One CSV line's cells ("quoted, cells" keep their commas).
fn cells(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cell.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cell).trim().to_string()),
            c => cell.push(c),
        }
    }
    out.push(cell.trim().to_string());
    out
}

fn parse_num(cell: &str) -> Option<f64> {
    let cleaned: String = cell.chars().filter(|c| !matches!(c, '$' | '€' | '£' | '%' | '_' | ' ')).collect();
    let cleaned = if cleaned.contains(',') && !cleaned.contains('.') && cleaned.split(',').skip(1).all(|g| g.len() == 3) { cleaned.replace(',', "") } else { cleaned };
    cleaned.parse::<f64>().ok().filter(|n| n.is_finite())
}

const LABEL_COLS: &[&str] = &["label", "name", "id", "country", "city", "category", "item", "team", "product", "task", "token", "milestone"];
const TEXT_COLS: &[&str] = &["text", "note", "event", "caption", "description", "title"];
const X_COLS: &[&str] = &["x", "lon", "lng", "longitude"];
const Y_COLS: &[&str] = &["y", "lat", "latitude"];
const OTHER_COLS: &[&str] = &["value", "r", "size", "group", "delta", "change", "region", "continent", "trend", "start", "end", "from", "to", "due", "status", "pick"];

/// CSV with or without a header row. Known columns (`label value x y r
/// group text delta region`, and their synonyms) fill their fields; the
/// other number columns are series.
fn csv_tables(text: &str, split: Option<&str>) -> Result<Vec<Table>, String> {
    let mut rows: Vec<Vec<String>> = text.lines().filter(|l| !l.trim().is_empty()).map(cells).collect();
    if rows.is_empty() {
        return Ok(vec![Table::default()]);
    }
    let header: Vec<String> = if rows[0].iter().all(|c| parse_num(c).is_none()) && rows.len() > 1 {
        rows.remove(0).into_iter().map(|h| h.to_lowercase()).collect()
    } else {
        // No header: label (or date), then values, and a text column.
        let width = rows.iter().map(Vec::len).max().unwrap_or(0);
        (0..width).map(|c| {
            let numeric = rows.iter().all(|r| r.get(c).map_or(true, |cell| parse_num(cell).is_some()));
            match (c, numeric) {
                (0, _) => "label".to_string(),
                (_, true) => format!("#{c}"),
                (_, false) => "text".to_string(),
            }
        }).collect()
    };
    let find = |names: &[&str]| header.iter().position(|h| names.contains(&h.as_str()));
    let split_col = split.and_then(|s| header.iter().position(|h| h.eq_ignore_ascii_case(s)));
    if let (Some(split), None) = (split, split_col) {
        return Err(format!("`split: \"{split}\"` names no CSV column (columns: {})", header.join(", ")));
    }
    let numeric = |c: usize| rows.iter().all(|r| r.get(c).map_or(true, |cell| cell.is_empty() || parse_num(cell).is_some()));
    // The label: a named label column, else the first text column that is
    // not the split, not the event text.
    let label_c = find(LABEL_COLS).or_else(|| find(&["date", "year", "period", "time", "month"]).filter(|c| Some(*c) != split_col)).or_else(|| {
        (0..header.len()).find(|&c| Some(c) != split_col && !numeric(c) && !TEXT_COLS.contains(&header[c].as_str()))
    });
    let text_c = find(TEXT_COLS);
    let (x_c, y_c) = (find(X_COLS), find(Y_COLS));
    let value_c = find(&["value", "count", "amount", "total"]);
    let known = |c: usize| {
        Some(c) == label_c || Some(c) == split_col || Some(c) == text_c || Some(c) == x_c || Some(c) == y_c || Some(c) == value_c
            || OTHER_COLS.contains(&header[c].as_str()) || LABEL_COLS.contains(&header[c].as_str())
    };
    let series_c: Vec<usize> = (0..header.len()).filter(|&c| !known(c) && numeric(c)).collect();
    let col = |names: &[&str]| find(names);
    let (r_c, group_c, delta_c, region_c) = (col(&["r", "size"]), col(&["group"]), col(&["delta", "change"]), col(&["region", "continent"]));
    let (start_c, end_c, status_c, pick_c) = (col(&["start", "from", "begin"]), col(&["end", "to", "due", "finish"]), col(&["status", "state"]), col(&["pick"]));
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let get = |c: Option<usize>| c.and_then(|c| row.get(c)).filter(|v| !v.is_empty());
        let numv = |c: Option<usize>| get(c).and_then(|v| parse_num(v));
        let series: Vec<f64> = series_c.iter().map(|&c| numv(Some(c)).unwrap_or(0.0)).collect();
        let value = numv(value_c).or_else(|| series.first().copied()).or_else(|| numv(y_c));
        let value = match (value, text_c) {
            (Some(v), _) => v,
            (None, Some(_)) => 0.0,
            (None, None) if x_c.is_some() || y_c.is_some() || start_c.is_some() => 0.0,
            (None, None) => return Err(format!("CSV row {} has no number in its value column", i + 1)),
        };
        let datum = Datum {
            label: get(label_c).cloned().unwrap_or_default(),
            value,
            values: if series.len() > 1 { series } else { vec![value] },
            x: numv(x_c),
            y: numv(y_c),
            r: numv(r_c),
            group: get(group_c).cloned(),
            text: get(text_c).cloned(),
            delta: numv(delta_c),
            trend: Vec::new(),
            region: get(region_c).cloned(),
            format: None,
            start: numv(start_c),
            end: numv(end_c),
            status: get(status_c).cloned(),
            pick: get(pick_c).is_some_and(|v| matches!(v.to_lowercase().as_str(), "true" | "yes" | "1" | "x")),
        };
        let fields = header.iter().cloned().zip(row.iter().cloned()).collect();
        out.push(Row { datum, fields });
    }
    let series: Vec<String> = if series_c.len() > 1 { series_c.iter().map(|&c| header[c].clone()).collect() } else { Vec::new() };
    let mut tables = split_rows(out, split);
    for table in &mut tables {
        table.series = series.clone();
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_b<R>(f: impl FnOnce(&B) -> R) -> R {
        let mut host = ScriptVmHost::new((), ());
        let mut vm = ScriptVm {
            host: host.host_context(),
            bx: Box::new(ScriptVmBase::new()),
        };
        let b = B::new(&mut vm);
        f(&b)
    }

    fn text(t: &str) -> Vec<Table> {
        with_b(|b| tables_of_text(b, t, None).unwrap())
    }

    #[test]
    fn csv_json_and_series_parse() {
        let d = &text("month,sales\nJan,3\nFeb,5.5\n")[0].rows;
        assert_eq!((d[1].label.as_str(), d[1].value), ("Feb", 5.5));
        assert_eq!(text("Jan,3\nFeb,4")[0].rows.len(), 2);
        let d = &text(r#"[{"label": "a", "value": 2}, {"label": "b", "value": 3, "group": "x"}]"#)[0].rows;
        assert_eq!((d[1].value, d[1].group.as_deref()), (3.0, Some("x")));
        assert!(with_b(|b| tables_of_text(b, "a,b\nx,y\nz,w", None).is_err()));
        let t = &text("quarter,north,south,east\nQ1,3,4,\"1,200\"\nQ2,5,2,7")[0];
        assert_eq!(t.series, vec!["north", "south", "east"]);
        assert_eq!(t.rows[0].values, vec![3.0, 4.0, 1200.0]);
        // A timeline: a date and its event, no value.
        let t = &text("2019,Founded in a garage\n2021,\"Series A, $12M\"")[0];
        assert_eq!((t.rows[1].label.as_str(), t.rows[1].text.as_deref()), ("2021", Some("Series A, $12M")));
    }

    #[test]
    fn a_long_table_splits_into_datasets() {
        let t = with_b(|b| tables_of_text(b, "year,name,value\n2019,A,3\n2019,B,5\n2020,A,6\n2020,B,4", Some("year")).unwrap());
        assert_eq!(t.len(), 2);
        assert_eq!(t[1].key.as_deref(), Some("2020"));
        assert_eq!((t[1].rows[0].label.as_str(), t[1].rows[0].value), ("A", 6.0));
        assert!(with_b(|b| tables_of_text(b, "a,b\nx,1", Some("year")).is_err()));
    }
}

//! The tweaker's Layout values and property chunks.
use super::PropertySets;
use makepad_widgets::design_data::props::fmt_f64;
use std::collections::BTreeMap;
#[path = "layout_view.rs"]
pub(in crate::tweak) mod view;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum BoxKind {
    Margin,
    Padding,
}

impl BoxKind {
    fn prop(self) -> &'static str {
        match self {
            BoxKind::Margin => "margin",
            BoxKind::Padding => "padding",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum GroupKind {
    Item,
    Container,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::tweak) enum LayoutKind {
    Identity,
    Group(GroupKind),
    Size,
    BoxInset(BoxKind),
    FlowSpacing,
    AlignGrid,
    GridTracks,
    Absolute,
}

#[derive(Clone, Copy)]
pub(in crate::tweak) enum LayoutClick {
    Size(&'static str, usize),
    Direction(FlowDir),
    Wrap,
    RowAlign(RowAlignText),
    Variant(&'static str, &'static str, &'static str),
    Justify(f64),
    Cross(f64),
}

#[derive(Clone, Default, PartialEq)]
pub(in crate::tweak) struct LayoutValues {
    pub(super) values: BTreeMap<String, String>,
    pub(super) originals: BTreeMap<String, String>,
    pub(super) is_grid: bool,
}

/// One axis of a size, as the Props tab reads it off a reflected row. The
/// row's text is whatever `fmt_enum` wrote -- `Size.Fill{weight: 100 ..}`,
/// `Size.Fixed(200)`, `Fit`, a bare number from an older row, `"50%"` or
/// `"calc(100% - 20px)"` for the CSS spellings -- and every one of them
/// has to come back as the same handful of shapes the editor draws.
#[derive(Clone, Debug, PartialEq)]
enum SizeText {
    Fill(FillText),
    Fit,
    Fixed(f64),
    /// `50%`, `25vw`, `60cqw`: a size relative to something, as written.
    Rel(String),
    /// `calc(..)`, `min(..)`, `clamp(..)`: a size the layout pass works out.
    Expr(String),
}

/// What a `Fill` carries: how much of the free space it takes against its
/// siblings, where it starts from, how it gives way, and the margin-box
/// bounds a Fill has always had. Read off the printed row and written back
/// whole, since a field of a variant is not a property of its own.
#[derive(Clone, Debug, PartialEq)]
struct FillText {
    weight: f64,
    /// `0`, `25%`, `calc(100% - 20px)`: the basis as written.
    basis: String,
    shrink: f64,
    min: Option<f64>,
    max: Option<f64>,
}

impl FillText {
    /// A Fill with nothing said about it.
    fn plain() -> Self {
        FillText { weight: 100.0, basis: "0".to_string(), shrink: 0.0, min: None, max: None }
    }

    /// The chunk that sets the axis to this Fill.
    fn chunk(&self, axis: &str) -> String {
        let mut out = format!(
            "{axis}: Size.Fill{{weight: {} basis: {} shrink: {}",
            fmt_f64(self.weight),
            size_literal(&self.basis),
            fmt_f64(self.shrink)
        );
        if let Some(min) = self.min {
            out.push_str(&format!(" min: {}", fmt_f64(min)));
        }
        if let Some(max) = self.max {
            out.push_str(&format!(" max: {}", fmt_f64(max)));
        }
        out.push('}');
        out
    }
}

/// A size as the engine reads it: a number stays bare, anything else --
/// `25%`, `calc(100% - 20px)` -- is a string for its parser.
fn size_literal(text: &str) -> String {
    let text = text.trim().trim_matches('"').trim();
    if text.parse::<f64>().is_ok() {
        text.to_string()
    } else {
        format!("\"{text}\"")
    }
}

/// The fields of a printed `Size.Fill{weight: 100 basis: FitBound.Abs(0)
/// shrink: 0}`; anything missing is the engine's own default.
fn parse_fill_text(text: &str) -> FillText {
    FillText {
        weight: field_number(text, "weight").unwrap_or(100.0),
        basis: fill_field(text, "basis").map(bound_text).unwrap_or_else(|| "0".to_string()),
        shrink: field_number(text, "shrink").unwrap_or(0.0),
        min: field_number(text, "min"),
        max: field_number(text, "max"),
    }
}

/// One field of a printed Fill, up to the next field or the closing brace;
/// a quoted value -- an expression with spaces in it -- is taken whole.
fn fill_field<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let key = format!("{field}: ");
    let start = text.find(&key)? + key.len();
    let rest = &text[start..];
    if let Some(quoted) = rest.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some(&quoted[..end]);
    }
    let end = [" weight:", " basis:", " shrink:", " min:", " max:", "}"]
        .iter()
        .filter_map(|stop| rest.find(stop))
        .min()
        .unwrap_or(rest.len());
    Some(rest[..end].trim())
}

/// A bound as printed -- `FitBound.Abs(120)`, `50%`, `"calc(100% - 20px)"`
/// -- as the text of its field: `120`, `50%`, `calc(100% - 20px)`.
fn bound_text(printed: &str) -> String {
    let text = printed.trim().trim_matches('"').trim();
    if text == "null" {
        return String::new(); // cleared: the row stays, the bound is gone
    }
    let head = text.split(['{', '(']).next().unwrap_or("").trim();
    if head.rsplit('.').next() == Some("Abs") {
        let inner = text.split('(').nth(1).unwrap_or("").trim_end_matches(')').trim();
        return inner.parse::<f64>().map(fmt_f64).unwrap_or_else(|_| inner.to_string());
    }
    text.to_string()
}

/// What was typed into a min / max field: nothing clears the bound, a
/// number is points, and a spelling the engine parses goes in quotes.
/// Fill and Fit mean nothing for a bound, and half-typed text is not sent.
fn bound_chunk(prop: &str, typed: &str) -> Option<String> {
    let typed = typed.trim().trim_matches('"').trim();
    if typed.is_empty() || typed == "-" || typed == "\u{2013}" || typed.eq_ignore_ascii_case("none") {
        return Some(format!("{prop}: nil"));
    }
    match parse_size_text(typed) {
        SizeText::Fixed(v) => Some(format!("{prop}: {}", fmt_f64(v))),
        SizeText::Rel(text) | SizeText::Expr(text) => Some(format!("{prop}: \"{text}\"")),
        SizeText::Fill(_) | SizeText::Fit => None,
    }
}

/// What was typed into the aspect field: nothing clears it, `16:9` or
/// `16/9` is a ratio, a number is width over height; `3:` on the way to
/// `3:2` is not sent.
fn aspect_chunk(typed: &str) -> Option<String> {
    let typed = typed.trim();
    if typed.is_empty() || typed == "-" || typed == "\u{2013}" || typed.eq_ignore_ascii_case("none") {
        return Some("aspect: nil".to_string());
    }
    if let Some((w, h)) = typed.split_once([':', '/']) {
        let (w, h) = (w.trim().parse::<f64>().ok()?, h.trim().parse::<f64>().ok()?);
        return (h != 0.0).then(|| format!("aspect: {}", fmt_f64(w / h)));
    }
    typed.parse::<f64>().ok().map(|v| format!("aspect: {}", fmt_f64(v)))
}

impl SizeText {
    /// The value the field shows for it.
    fn field_text(&self) -> String {
        match self {
            SizeText::Fill(_) => "Fill".to_string(),
            SizeText::Fit => "Fit".to_string(),
            SizeText::Fixed(v) => fmt_f64(*v),
            SizeText::Rel(text) | SizeText::Expr(text) => text.clone(),
        }
    }
}

/// Read a size row's text. Tolerant of every spelling the panel has ever
/// shown for one, because rows come from `fmt_value` today and came from
/// dotted fragments and bare numbers before it.
fn parse_size_text(text: &str) -> SizeText {
    let text = text.trim().trim_matches('"').trim();
    if text.is_empty() {
        return SizeText::Fit;
    }
    if let Ok(v) = text.parse::<f64>() {
        return SizeText::Fixed(v);
    }
    // `Size.Fill{weight: 100 ..}`, `Fill{..}`, `Fill`, and the Fit / Fixed
    // shapes: the variant is the word after the last dot before any brace
    // or paren. Tried FIRST, because a printed Fill carries `Abs(0)` inside
    // it and a paren alone would read as an expression.
    let head = text.split(['{', '(']).next().unwrap_or("").trim();
    let variant = head.rsplit('.').next().unwrap_or("").trim();
    match variant {
        "Fill" => return SizeText::Fill(parse_fill_text(text)),
        "Fit" => return SizeText::Fit,
        "Fixed" => {
            // `Fixed(` on the way to `Fixed(300)` is not a size yet, and
            // certainly not a size of nothing.
            let inner = text.split('(').nth(1).unwrap_or("").trim_end_matches(')').trim();
            return match inner.parse() {
                Ok(v) => SizeText::Fixed(v),
                Err(_) => SizeText::Fit,
            };
        }
        _ => {}
    }
    let lower = text.to_ascii_lowercase();
    // An expression is kept as written and emitted back as the same
    // string -- once it is one: `calc(` on the way to `calc(100% - 20px)`
    // reads as nothing, so nothing is sent for it.
    if lower.contains('(') {
        let balanced = lower.matches('(').count() == lower.matches(')').count();
        return if balanced && lower.ends_with(')') {
            SizeText::Expr(text.to_string())
        } else {
            SizeText::Fit
        };
    }
    if let Some(num) = lower
        .strip_suffix("cqw")
        .or_else(|| lower.strip_suffix("cqh"))
        .or_else(|| lower.strip_suffix("vw"))
        .or_else(|| lower.strip_suffix("vh"))
        .or_else(|| lower.strip_suffix('%'))
        .or_else(|| lower.strip_suffix("px"))
    {
        if num.trim().parse::<f64>().is_ok() {
            // The unit goes lowercase: the engine's parser knows `vw`,
            // not `VW`, and the field shows what was accepted.
            return if lower.ends_with("px") {
                SizeText::Fixed(num.trim().parse().unwrap_or(0.0))
            } else {
                SizeText::Rel(lower.clone())
            };
        }
    }
    SizeText::Fit
}

/// `weight: 100` out of a printed named variant, when the field is a plain
/// number.
fn field_number(text: &str, field: &str) -> Option<f64> {
    let key = format!("{field}: ");
    let start = text.find(&key)? + key.len();
    let rest = &text[start..];
    let end = rest.find([' ', '}', ',']).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The direction a container lays its children out in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::tweak) enum FlowDir {
    Right,
    Down,
    Overlay,
}

/// How the walks in one wrapped row line up vertically.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(in crate::tweak) enum RowAlignText {
    Top,
    Center,
    Bottom,
}

impl RowAlignText {
    fn name(self) -> &'static str {
        match self {
            RowAlignText::Top => "Top",
            RowAlignText::Center => "Center",
            RowAlignText::Bottom => "Bottom",
        }
    }
}

/// A `flow` row as the Props tab reads it: `Flow.Right{row_align:
/// RowAlign.Top wrap: true}`, `Flow.Down`, or the bare `Right` of a
/// hand-written source. The wrap and the row alignment only mean anything
/// for `Right`; they are kept across a change of direction so that going
/// Down and back does not lose them.
#[derive(Clone, Copy, Debug, PartialEq)]
struct FlowText {
    dir: FlowDir,
    wrap: bool,
    row_align: RowAlignText,
}

impl FlowText {
    /// Children go left to right and onto a new row when the width runs out.
    fn wraps(&self) -> bool {
        self.dir == FlowDir::Right && self.wrap
    }

    fn with_dir(self, dir: FlowDir) -> Self {
        FlowText { dir, ..self }
    }

    /// The wrap button: on a Right flow it toggles; on any other it turns
    /// the flow into a wrapping Right one, which is what pressing "wrap"
    /// on a Down container can only mean.
    fn toggled_wrap(self) -> Self {
        FlowText { dir: FlowDir::Right, wrap: !self.wraps(), ..self }
    }

    fn with_row_align(self, row_align: RowAlignText) -> Self {
        FlowText { dir: FlowDir::Right, row_align, ..self }
    }

    /// The chunk that sets it: always the whole value. A field of a
    /// variant is not a property, so `flow.wrap: true` is never emitted.
    fn chunk(&self) -> String {
        match self.dir {
            FlowDir::Down => "flow: Flow.Down".to_string(),
            FlowDir::Overlay => "flow: Flow.Overlay".to_string(),
            FlowDir::Right => format!(
                "flow: Flow.Right{{wrap: {} row_align: RowAlign.{}}}",
                self.wrap,
                self.row_align.name()
            ),
        }
    }
}

/// Read a flow row's text, in every spelling it has had: the printed
/// `Flow.Right{row_align: RowAlign.Top wrap: true}`, a bare `Down`, the
/// `Right{wrap: true}` of a source, and the `RightWrap` an older panel
/// wrote. Anything else reads as Down, the commonest container.
fn parse_flow_text(text: &str) -> FlowText {
    let text = text.trim();
    let head = text.split(['{', '(']).next().unwrap_or("").trim();
    let variant = head.rsplit('.').next().unwrap_or("").trim();
    let dir = match variant {
        "Right" | "RightWrap" => FlowDir::Right,
        "Overlay" => FlowDir::Overlay,
        _ => FlowDir::Down,
    };
    let wrap = variant == "RightWrap" || text.contains("wrap: true");
    let row_align = match field_word(text, "row_align") {
        Some("Center") => RowAlignText::Center,
        Some("Bottom") => RowAlignText::Bottom,
        _ => RowAlignText::Top,
    };
    FlowText { dir, wrap, row_align }
}

/// `row_align: RowAlign.Center` out of a printed named variant, as the bare
/// variant word after any enum prefix.
fn field_word<'a>(text: &'a str, field: &str) -> Option<&'a str> {
    let key = format!("{field}: ");
    let start = text.find(&key)? + key.len();
    let rest = &text[start..];
    let end = rest.find([' ', '}', ',']).unwrap_or(rest.len());
    Some(rest[..end].rsplit('.').next().unwrap_or(""))
}

/// The strings out of a printed list -- `["70px" "20%" "1fr"]` -- joined
/// with `sep`; a list that is not strings comes back as it was printed.
fn quoted_list_text(printed: &str, sep: &str) -> String {
    let inner = printed.trim().trim_start_matches('[').trim_end_matches(']').trim();
    if inner.is_empty() {
        return String::new();
    }
    if !inner.starts_with('"') {
        return inner.to_string();
    }
    let mut out = Vec::new();
    let mut rest = inner;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        out.push(&after[..end]);
        rest = &after[end + 1..];
    }
    out.join(sep)
}

/// Split a track list on spaces, keeping `minmax(60px, 1fr)` together.
fn split_tracks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut cur = String::new();
    for ch in text.chars() {
        match ch {
            '(' => {
                depth += 1;
                cur.push(ch);
            }
            ')' => {
                depth = depth.saturating_sub(1);
                cur.push(ch);
            }
            ' ' | '\t' | ',' if depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A track length as the engine's parser takes it: a number, or one with
/// `px`, `%` or `fr`, or a balanced expression such as `calc(...)`.
fn track_len_ok(text: &str) -> bool {
    let text = text.trim();
    if text.is_empty() {
        return false;
    }
    let bare = text
        .strip_suffix("px")
        .or_else(|| text.strip_suffix("fr"))
        .or_else(|| text.strip_suffix('%'))
        .unwrap_or(text);
    if bare.trim().parse::<f64>().is_ok_and(|v| v >= 0.0) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    ["calc(", "min(", "max(", "clamp("].iter().any(|f| lower.starts_with(f))
        && lower.ends_with(')')
        && lower.matches('(').count() == lower.matches(')').count()
}

/// A function's arguments, split on the commas at its own depth, so
/// `minmax(min(10px, 5%), 1fr)` gives two, the way the engine reads it.
fn split_args(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in body.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(body[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    out.push(body[start..].trim());
    out
}

/// One track as the engine's parser takes it: a length, `minmax(a, b)`, or
/// `repeat(n | auto-fill | auto-fit, minmax(a, b))`.
fn track_ok(text: &str) -> bool {
    let text = text.trim();
    if let Some(body) = text.strip_prefix("minmax(").and_then(|b| b.strip_suffix(')')) {
        let parts = split_args(body);
        return parts.len() == 2 && parts.iter().all(|p| track_len_ok(p));
    }
    if let Some(body) = text.strip_prefix("repeat(").and_then(|b| b.strip_suffix(')')) {
        let parts = split_args(body);
        if parts.len() != 2 {
            return false;
        }
        let (count, segment) = (parts[0], parts[1]);
        let count_ok = count == "auto-fill" || count == "auto-fit" || count.parse::<u32>().is_ok();
        return count_ok && segment.starts_with("minmax(") && track_ok(segment);
    }
    track_len_ok(text)
}

/// What was typed into a tracks field, as the list the engine takes; a
/// half-typed track is not sent, and no tracks at all is an empty list.
fn tracks_chunk(prop: &str, typed: &str) -> Option<String> {
    let tracks = split_tracks(typed);
    if !tracks.iter().all(|t| track_ok(t)) {
        return None;
    }
    let quoted: Vec<String> = tracks.iter().map(|t| format!("\"{}\"", t.trim())).collect();
    Some(format!("{prop}: [{}]", quoted.join(" ")))
}

/// What was typed into the areas field: rows separated by `/`, each a row
/// of names with `.` for an empty cell. Rows of unequal length are a row
/// still being typed, and are not sent.
fn areas_chunk(typed: &str) -> Option<String> {
    let rows: Vec<String> = typed
        .split('/')
        .map(|row| row.trim().replace('"', ""))
        .filter(|row| !row.is_empty())
        .collect();
    let width = rows.first().map(|row| row.split_whitespace().count());
    if rows.iter().any(|row| Some(row.split_whitespace().count()) != width) {
        return None;
    }
    let quoted: Vec<String> = rows.iter().map(|row| format!("\"{row}\"")).collect();
    Some(format!("areas: [{}]", quoted.join(" ")))
}

/// Where a child sits in its Grid, as the rows say: 0 is wherever the
/// fill order puts it.
#[derive(Clone, Debug, Default, PartialEq)]
struct CellText {
    col: u32,
    row: u32,
    col_span: u32,
    row_span: u32,
    area: String,
}

impl CellText {
    /// The chunk that places it; a cell that says nothing is no cell.
    fn chunk(&self) -> String {
        if self.col == 0 && self.row == 0 && self.col_span == 0 && self.row_span == 0 && self.area.is_empty() {
            return "cell: nil".to_string();
        }
        let area = if self.area.is_empty() { "nil".to_string() } else { format!("@{}", self.area) };
        format!(
            "cell: CellPlacement{{col: {} row: {} col_span: {} row_span: {} area: {area}}}",
            self.col, self.row, self.col_span, self.row_span
        )
    }
}

/// A container name as the chunk that sets it: an id, so letters, digits
/// and underscores, not starting with a digit; anything else is not sent.
fn container_chunk(typed: &str) -> Option<String> {
    let name = typed.trim().trim_start_matches('@');
    let valid = !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    valid.then(|| format!("container_id: @{name}"))
}

/// `vec2f(10 20)` as printed, or `vec2(10, 20)` as written, to its numbers.
fn parse_vec2_text(text: &str) -> Option<(f64, f64)> {
    let inner = text.split('(').nth(1)?.trim_end_matches(')');
    let mut parts = inner.split([' ', ',']).filter(|p| !p.is_empty());
    let x: f64 = parts.next()?.parse().ok()?;
    let y: f64 = parts.next()?.parse().ok()?;
    Some((x, y))
}

/// What a person typed into a size field, as the chunk that sets it. A bare
/// word is a mode, a number is points, a percent or a viewport unit or a
/// function is the CSS spelling and goes in quotes so the engine's string
/// parser sees it. Text that reads as none of those -- the half of a word
/// still being typed -- is not sent.
fn size_chunk(axis: &str, typed: &str) -> Option<String> {
    let typed = typed.trim().trim_matches('"').trim();
    match typed.to_ascii_lowercase().as_str() {
        "fill" => return Some(format!("{axis}: Fill")),
        "fit" => return Some(format!("{axis}: Fit")),
        _ => {}
    }
    match parse_size_text(typed) {
        SizeText::Fixed(v) => Some(format!("{axis}: {}", fmt_f64(v))),
        SizeText::Rel(text) | SizeText::Expr(text) => Some(format!("{axis}: \"{text}\"")),
        SizeText::Fill(_) => Some(format!("{axis}: Fill")),
        SizeText::Fit => None,
    }
}

impl LayoutValues {
    fn row_value(&self, prop: &str) -> Option<&str> {
        self.values.get(prop).map(String::as_str)
    }

    /// The selection's cell as the rows say it.
    fn cell_text(&self) -> CellText {
        let num = |key: &str| {
            self.row_value(&format!("cell.{key}"))
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0)
                .max(0.0) as u32
        };
        CellText {
            col: num("col"),
            row: num("row"),
            col_span: num("col_span"),
            row_span: num("row_span"),
            area: self
                .row_value("cell.area")
                .map(|v| v.trim_start_matches('@'))
                .filter(|v| *v != "-")
                .unwrap_or("")
                .to_string(),
        }
    }

    /// The cell with one field -- col, row, a span, the area -- replaced by
    /// what was typed, as the chunk that places the whole of it. An area
    /// that is not an id is not sent.
    fn cell_chunk(&self, key: &str, typed: &str) -> Option<String> {
        let mut cell = self.cell_text();
        match key {
            "area" => {
                let name = typed.trim().trim_start_matches('@');
                let valid = name.is_empty()
                    || (name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                        && !name.starts_with(|c: char| c.is_ascii_digit()));
                if !valid {
                    return None;
                }
                cell.area = name.to_string();
            }
            _ => {
                let value = typed.trim().parse::<f64>().ok()?.max(0.0) as u32;
                match key {
                    "col" => cell.col = value,
                    "row" => cell.row = value,
                    "col_span" => cell.col_span = value,
                    "row_span" => cell.row_span = value,
                    _ => return None,
                }
            }
        }
        Some(cell.chunk())
    }

    /// The absolute position with one coordinate replaced.
    fn abs_chunk(&self, key: &str, value: f64, pending: Option<&str>) -> String {
        let (x, y) = pending.or_else(|| self.row_value("abs_pos")).and_then(parse_vec2_text).unwrap_or((0.0, 0.0));
        let (x, y) = if key == "x" { (value, y) } else { (x, value) };
        format!("abs_pos: vec2({}, {})", fmt_f64(x), fmt_f64(y))
    }

    /// The axis's Fill with one field -- weight, shrink, basis -- replaced
    /// by what was typed, as the chunk that sets the whole of it.
    fn fill_chunk(&self, axis: &str, key: &str, typed: &str, pending: Option<&str>) -> String {
        let mut fill = match parse_size_text(pending.or_else(|| self.row_value(axis)).unwrap_or("")) {
            SizeText::Fill(fill) => fill,
            _ => FillText::plain(),
        };
        match key {
            "weight" => fill.weight = typed.trim().parse().unwrap_or(fill.weight),
            "shrink" => fill.shrink = typed.trim().parse().unwrap_or(fill.shrink),
            "basis" => fill.basis = typed.trim().trim_matches('"').trim().to_string(),
            _ => {}
        }
        fill.chunk(axis)
    }
}

impl LayoutKind {
    pub(super) fn owns(self, prop: &str) -> bool {
        let base = prop.split('.').next().unwrap_or(prop);
        match self {
            Self::Size => matches!(base, "width" | "height" | "min_width" | "max_width" | "min_height" | "max_height" | "aspect"),
            Self::BoxInset(kind) => kind.prop() == base,
            Self::FlowSpacing => matches!(base, "flow" | "spacing" | "wrap_spacing"),
            Self::AlignGrid => matches!(base, "align" | "distribute" | "align_items" | "justify_items"),
            Self::GridTracks => matches!(base, "columns" | "rows" | "areas" | "column_gap" | "row_gap" | "auto_flow"),
            Self::Absolute => base == "abs_pos",
            Self::Identity | Self::Group(_) => false,
        }
    }
}

pub(in crate::tweak) enum LayoutEvent<'a> {
    Click(LayoutClick),
    Absolute(bool),
    Number(&'a str, &'a makepad_widgets::fab_controls::FabValueInputAction),
    Returned(&'a str, &'a str),
    Reset(&'a str),
}

impl LayoutValues {
    fn click_chunk(&self, click: LayoutClick) -> String {
        let flow = || parse_flow_text(self.row_value("flow").unwrap_or(""));
        match click {
            LayoutClick::Size(axis, mode) => match mode {
                0 => format!("{axis}: Fill"),
                1 => format!("{axis}: Fit"),
                _ => {
                    let fixed = match parse_size_text(self.row_value(axis).unwrap_or("")) {
                        SizeText::Fixed(v) => fmt_f64(v),
                        _ => "100".into(),
                    };
                    format!("{axis}: {fixed}")
                }
            },
            LayoutClick::Direction(dir) => flow().with_dir(dir).chunk(),
            LayoutClick::Wrap => flow().toggled_wrap().chunk(),
            LayoutClick::RowAlign(align) => flow().with_row_align(align).chunk(),
            LayoutClick::Variant(prop, ty, variant) => format!("{prop}: {ty}.{variant}"),
            LayoutClick::Justify(value) | LayoutClick::Cross(value) => {
                let main = matches!(click, LayoutClick::Justify(_));
                let down = flow().dir == FlowDir::Down;
                let axis = if main == down { "align.y" } else { "align.x" };
                format!("{axis}: {}", fmt_f64(value))
            }
        }
    }

    /// The named Sets in the order the shared app must apply them.
    pub(in crate::tweak) fn decision<'a>(&self, event: LayoutEvent<'a>, pending: Option<&str>) -> Option<PropertySets<'a>> {
        let clear_distribute = matches!(event, LayoutEvent::Click(LayoutClick::Justify(_)))
            && self.row_value("distribute").is_some_and(|v| v.rsplit('.').next() != Some("Start"));
        let mut sets = self.field_set(event, pending)?;
        if clear_distribute { sets.before = Some(("distribute", "Distribute.Start".into())); }
        Some(sets)
    }

    fn field_set<'a>(&self, event: LayoutEvent<'a>, pending: Option<&str>) -> Option<PropertySets<'a>> {
        use makepad_widgets::fab_controls::FabValueInputAction;
        let chunk = match event {
            LayoutEvent::Click(click) => self.click_chunk(click),
            LayoutEvent::Absolute(on) => if on { "abs_pos: vec2(0, 0)" } else { "abs_pos: nil" }.into(),
            LayoutEvent::Reset(field) | LayoutEvent::Number(field, FabValueInputAction::Reset) => {
                let prop = field.split('#').next()?;
                let old = self.originals.get(prop)?;
                let value = match old.as_str() { "null" | "@-" => "nil", v => v };
                return Some(PropertySets::one(prop, value.to_owned()));
            }
            LayoutEvent::Number(prop, FabValueInputAction::Changed(v) | FabValueInputAction::Ended(v)) => {
                if let Some((axis, key)) = prop.split_once('#') {
                    match axis {
                        "abs_pos" => self.abs_chunk(key, *v, pending),
                        "cell" => self.cell_chunk(key, &fmt_f64(*v))?,
                        _ => self.fill_chunk(axis, key, &fmt_f64(*v), pending),
                    }
                } else {
                    return Some(PropertySets::one(prop, fmt_f64(*v)));
                }
            }
            LayoutEvent::Number(_, _) => return None,
            LayoutEvent::Returned(prop, text) => {
                let text = text.trim();
                if prop == "width" || prop == "height" {
                    size_chunk(prop, text)?
                } else if let Some((axis, key)) = prop.split_once('#') {
                    if axis == "cell" { self.cell_chunk(key, text)? }
                    else { (!text.is_empty()).then(|| self.fill_chunk(axis, key, text, pending))? }
                } else if prop == "columns" || prop == "rows" {
                    tracks_chunk(prop, text)?
                } else if prop == "areas" {
                    areas_chunk(text)?
                } else if prop.starts_with("min_") || prop.starts_with("max_") {
                    bound_chunk(prop, text)?
                } else if prop == "aspect" {
                    aspect_chunk(text)?
                } else if prop == "container_id" {
                    if text.is_empty() { return self.field_set(LayoutEvent::Reset(prop), None); }
                    container_chunk(text)?
                } else {
                    return None;
                }
            }
        };
        let (prop, value) = chunk.split_once(": ")?;
        Some(PropertySets::one(prop.to_owned(), value.to_owned()))
    }
}

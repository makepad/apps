//! Changes to a board, written as patches to its source: each op rewrites
//! only the statements and fields it changes, so a hand-written or
//! AI-written board keeps its comments, order and layout. Every op is
//! checked on the loaded board first and the result is loaded again, so an
//! op never leaves a board that does not load.

use crate::model::*;
use crate::splash::{self, number, parse, string_literal, Parsed};

/// One change. Field values are given as the tools take them (`Patch`).
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    /// Add an item; its `id` is chosen when empty.
    Add(Item),
    /// Change fields of an item.
    Update { id: String, patch: Patch },
    /// Move and/or resize items: `(id, start, length, row)`.
    Place(Vec<(String, f64, f64, Option<String>)>),
    /// Remove items (and links that name them; generate `from`s that name
    /// them are cleared).
    Delete(Vec<String>),
    AddRow(Row),
    /// Remove a row and every item on it.
    DeleteRow(String),
    MoveRow { id: String, index: usize },
    RenameRow { id: String, label: String },
    Link(Link),
    Unlink(String),
    /// Header fields.
    Header(Header),
    /// Record (or clear) the cut built from the board.
    Built(Option<Built>),
    /// Replace the type-voice legend.
    Voices(Vec<TypeVoice>),
}

/// Header fields to set; `None` leaves one as it is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Header {
    pub title: Option<String>,
    pub duration: Option<f64>,
    /// `Some(None)` clears the tempo.
    pub bpm: Option<Option<f64>>,
    pub beat_offset: Option<f64>,
    pub music: Option<Option<String>>,
    pub brief: Option<String>,
    pub style: Option<String>,
    pub kit: Option<String>,
    pub format: Option<(u32, u32)>,
    pub edit: Option<Option<String>>,
    /// A frame layout template id (`shorts`), or `Some(None)` for none.
    pub layout: Option<Option<String>>,
}

/// Item fields to set: `(field, value)`; an empty string (or `None`)
/// removes an optional field.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Patch {
    pub fields: Vec<(String, Value)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Num(f64),
    Int(i64),
    Tag(String),
    Name(String),
    Color(u32),
    Colors(Vec<u32>),
    Asset(AssetRef),
    Bool(bool),
    /// Item ids written as tags (`[@a, @b]`).
    Tags(Vec<String>),
    Clear,
}

impl Value {
    fn literal(&self) -> Option<String> {
        Some(match self {
            Value::Text(text) if text.is_empty() => return None,
            Value::Text(text) => string_literal(text),
            Value::Num(n) => number(*n),
            Value::Int(n) => n.to_string(),
            Value::Tag(tag) => format!("@{tag}"),
            Value::Name(name) => name.clone(),
            Value::Color(color) => splash::color_literal(*color),
            Value::Colors(colors) if colors.is_empty() => return None,
            Value::Colors(colors) => format!("[{}]", colors.iter().map(|c| splash::color_literal(*c)).collect::<Vec<_>>().join(", ")),
            Value::Asset(asset) => asset_literal(asset),
            Value::Bool(false) => return None,
            Value::Bool(true) => "true".into(),
            Value::Tags(tags) if tags.is_empty() => return None,
            Value::Tags(tags) => format!("[{}]", tags.iter().map(|tag| format!("@{tag}")).collect::<Vec<_>>().join(", ")),
            Value::Clear => return None,
        })
    }
}

/// What an op did: the new source and board, and the items it touched.
#[derive(Clone, Debug)]
pub struct Applied {
    pub source: String,
    pub board: Storyboard,
    pub changed: Vec<String>,
}

fn stmt_of<'a>(parsed: &'a Parsed, name: &str) -> Option<&'a splash::Stmt> {
    parsed.stmts.iter().find(|stmt| stmt.name.as_deref() == Some(name))
}

fn root_of(parsed: &Parsed) -> Option<&splash::Stmt> {
    parsed.stmts.iter().rev().find(|stmt| stmt.name.is_none() && stmt.value.ty() == Some("Storyboard"))
}

/// The field names an item type takes.
pub fn fields_of(ty: &str) -> &'static [&'static str] {
    match ty {
        "Scene" => SCENE_FIELDS,
        "Note" => NOTE_FIELDS,
        "Chapter" => CHAPTER_FIELDS,
        "Treatment" => TREATMENT_FIELDS,
        "Page" => PAGE_FIELDS,
        "Lyric" => LYRIC_FIELDS,
        "Device" => DEVICE_FIELDS,
        _ => REF_FIELDS,
    }
}

/// Set (or with `None`, remove) `field` of the `let name` object.
fn set_let_field(src: &str, name: &str, field: &str, value: Option<&str>) -> Result<String, String> {
    let parsed = parse(src);
    let stmt = stmt_of(&parsed, name).ok_or_else(|| format!("there is no `let {name}` in the board's source"))?;
    Ok(match value {
        Some(value) => splash::set_field(src, &stmt.value, field, value),
        None => splash::remove_field(src, &stmt.value, field),
    })
}

fn set_root_field(src: &str, field: &str, value: Option<&str>) -> Result<String, String> {
    let parsed = parse(src);
    let root = root_of(&parsed).ok_or("the board has no final `Storyboard{…}`")?;
    Ok(match value {
        Some(value) => splash::set_field(src, &root.value, field, value),
        None => splash::remove_field(src, &root.value, field),
    })
}

/// Rewrite the root's `key: [names]` list with `f`.
fn edit_list(src: &str, key: &str, f: impl FnOnce(&mut Vec<String>)) -> Result<String, String> {
    let parsed = parse(src);
    let root = root_of(&parsed).ok_or("the board has no final `Storyboard{…}`")?;
    let (mut names, indent) = match root.value.field(key) {
        Some(node) => (node.arr().unwrap_or(&[]).iter().filter_map(|n| n.name().map(str::to_string)).collect::<Vec<_>>(), splash::indent_of(src, node.start)),
        None => (Vec::new(), "    ".to_string()),
    };
    f(&mut names);
    if names.is_empty() && key == "links" {
        return Ok(splash::remove_field(src, &root.value, key));
    }
    Ok(splash::set_field(src, &root.value, key, &splash::names_array(&names, &indent)))
}

/// Insert `let name = value` after the last statement of its group (rows
/// after rows, items after items, links last), before the final board.
fn insert_let(src: &str, name: &str, value: &str, after_types: &[&str]) -> Result<String, String> {
    let parsed = parse(src);
    let root = root_of(&parsed).ok_or("the board has no final `Storyboard{…}`")?;
    let anchor = parsed.stmts.iter().filter(|stmt| stmt.name.is_some() && stmt.start < root.start && stmt.value.ty().is_some_and(|ty| after_types.contains(&ty))).map(|stmt| stmt.end).max();
    let mut out = src.to_string();
    let line = format!("let {name} = {value}\n");
    match anchor {
        Some(end) => {
            let at = src[end..].find('\n').map_or(src.len(), |n| end + n + 1);
            if at >= src.len() && !src.ends_with('\n') {
                out.push('\n');
                out.push_str(&line);
            } else {
                out.insert_str(at, &line);
            }
        }
        None => {
            // Before the final board, with a blank line after.
            let at = src[..root.start].rfind('\n').map_or(0, |n| n + 1);
            out.insert_str(at, &format!("{line}\n"));
        }
    }
    Ok(out)
}

fn remove_let(src: &str, name: &str) -> String {
    let parsed = parse(src);
    match stmt_of(&parsed, name) {
        Some(stmt) => splash::remove_stmt(src, stmt),
        None => src.to_string(),
    }
}

fn check_span(start: f64, length: f64) -> Result<(), String> {
    if !(start.is_finite() && length.is_finite()) || start < 0.0 || length <= 0.0 {
        return Err(format!("a span is start ≥ 0 and length > 0 seconds (got start {start} and length {length})"));
    }
    Ok(())
}

/// Apply `op` to the board in `src`.
pub fn apply(src: &str, op: &Op) -> Result<Applied, String> {
    let (board, _) = Storyboard::load(src).map_err(|diags| format!("the board does not load: {}", diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")))?;
    let mut changed = Vec::new();
    let out = match op {
        Op::Add(item) => {
            let mut item = item.clone();
            if item.id.is_empty() {
                item.id = board.fresh_id(item.body.kind());
            }
            if !splash::valid_name(&item.id) {
                return Err(format!("`{}` is not a usable id: letters, digits and `_`, starting with a letter, not a Splash keyword", item.id));
            }
            if board.item(&item.id).is_some() || board.row(&item.id).is_some() || board.links.iter().any(|link| link.id == item.id) || board.voice(&item.id).is_some() {
                return Err(format!("`{}` is taken: pick another id or leave it out", item.id));
            }
            if board.row(&item.row).is_none() {
                return Err(format!("there is no row `{}` (rows: {})", item.row, row_names(&board)));
            }
            check_span(item.start, item.length)?;
            check_refs(&board, &item)?;
            let with_let = insert_let(src, &item.id, &item_source(&item), &["Row", "Scene", "Note", "Ref", "Chapter", "Treatment", "Page", "Lyric", "Device"])?;
            let id = item.id.clone();
            changed.push(id.clone());
            edit_list(&with_let, "items", |names| names.push(id))?
        }
        Op::Update { id, patch } => {
            let item = board.item(id).ok_or_else(|| format!("there is no item `{id}` (items: {})", item_names(&board)))?;
            let ty = item.body.ty();
            let mut out = src.to_string();
            for (field, value) in &patch.fields {
                if !fields_of(ty).contains(&field.as_str()) {
                    return Err(format!("a {} has no field `{field}`; it has {}", ty, fields_of(ty).join(", ")));
                }
                check_value(&board, item, field, value)?;
                out = set_let_field(&out, id, field, value.literal().as_deref())?;
            }
            changed.push(id.clone());
            out
        }
        Op::Place(places) => {
            let mut out = src.to_string();
            for (id, start, length, row) in places {
                let item = board.item(id).ok_or_else(|| format!("there is no item `{id}` (items: {})", item_names(&board)))?;
                check_span(*start, *length)?;
                if (item.start - start).abs() > 1e-9 || (item.length - length).abs() > 1e-9 {
                    out = set_let_field(&out, id, "span", Some(&format!("[{}, {}]", number(*start), number(*length))))?;
                }
                if let Some(row) = row {
                    if board.row(row).is_none() {
                        return Err(format!("there is no row `{row}` (rows: {})", row_names(&board)));
                    }
                    if *row != item.row {
                        out = set_let_field(&out, id, "row", Some(row))?;
                    }
                }
                changed.push(id.clone());
            }
            out
        }
        Op::Delete(ids) => {
            for id in ids {
                if board.item(id).is_none() {
                    return Err(format!("there is no item `{id}` (items: {})", item_names(&board)));
                }
            }
            let mut out = src.to_string();
            let links: Vec<String> = board.links.iter().filter(|link| ids.contains(&link.from) || ids.contains(&link.to)).map(|link| link.id.clone()).collect();
            for link in &links {
                out = remove_let(&out, link);
            }
            if !links.is_empty() {
                out = edit_list(&out, "links", |names| names.retain(|name| !links.contains(name)))?;
            }
            for item in &board.items {
                if ids.contains(&item.id) {
                    continue;
                }
                if let Body::Scene(scene) = &item.body {
                    if scene.gen.from.as_ref().is_some_and(|from| ids.contains(from)) {
                        out = set_let_field(&out, &item.id, "from", None)?;
                    }
                }
            }
            for item in &board.items {
                if let Body::Page(page) = &item.body {
                    if !ids.contains(&item.id) && page.refs.iter().any(|id| ids.contains(id)) {
                        let kept: Vec<String> = page.refs.iter().filter(|id| !ids.contains(id)).cloned().collect();
                        let value = Value::Tags(kept).literal();
                        out = set_let_field(&out, &item.id, "refs", value.as_deref())?;
                    }
                }
            }
            if board.music.as_ref().is_some_and(|music| ids.contains(music)) {
                out = set_root_field(&out, "music", None)?;
            }
            for id in ids {
                out = remove_let(&out, id);
                changed.push(id.clone());
            }
            edit_list(&out, "items", |names| names.retain(|name| !ids.contains(name)))?
        }
        Op::AddRow(row) => {
            let mut row = row.clone();
            if row.id.is_empty() {
                row.id = if board.row(row.kind.tag()).is_none() && board.item(row.kind.tag()).is_none() { row.kind.tag().to_string() } else { board.fresh_id(row.kind.tag()) };
            }
            if !splash::valid_name(&row.id) || board.row(&row.id).is_some() || board.item(&row.id).is_some() {
                return Err(format!("`{}` is not a free row id", row.id));
            }
            let with_let = insert_let(src, &row.id, &row_source(&row), &["Row"])?;
            let id = row.id.clone();
            edit_list(&with_let, "rows", |names| names.push(id))?
        }
        Op::DeleteRow(id) => {
            if board.row(id).is_none() {
                return Err(format!("there is no row `{id}` (rows: {})", row_names(&board)));
            }
            let on_it: Vec<String> = board.items.iter().filter(|item| item.row == *id).map(|item| item.id.clone()).collect();
            let mut out = src.to_string();
            if !on_it.is_empty() {
                out = apply(&out, &Op::Delete(on_it.clone()))?.source;
                changed.extend(on_it);
            }
            out = remove_let(&out, id);
            edit_list(&out, "rows", |names| names.retain(|name| name != id))?
        }
        Op::MoveRow { id, index } => {
            if board.row(id).is_none() {
                return Err(format!("there is no row `{id}` (rows: {})", row_names(&board)));
            }
            edit_list(src, "rows", |names| {
                names.retain(|name| name != id);
                let index = (*index).min(names.len());
                names.insert(index, id.clone());
            })?
        }
        Op::RenameRow { id, label } => {
            if board.row(id).is_none() {
                return Err(format!("there is no row `{id}` (rows: {})", row_names(&board)));
            }
            set_let_field(src, id, "label", Some(&string_literal(label)))?
        }
        Op::Link(link) => {
            let mut link = link.clone();
            if board.item(&link.from).is_none() || board.item(&link.to).is_none() {
                return Err(format!("a link joins two items; `{}` or `{}` is not one (items: {})", link.from, link.to, item_names(&board)));
            }
            if link.from == link.to {
                return Err("a link joins two different items".into());
            }
            if link.id.is_empty() {
                link.id = board.fresh_id("link");
            }
            let with_let = insert_let(src, &link.id, &link_source(&link), &["Row", "Scene", "Note", "Ref", "Chapter", "Treatment", "Page", "Lyric", "Device", "Link"])?;
            let id = link.id.clone();
            changed.extend([link.from.clone(), link.to.clone()]);
            edit_list(&with_let, "links", |names| names.push(id))?
        }
        Op::Unlink(id) => {
            if !board.links.iter().any(|link| link.id == *id) {
                return Err(format!("there is no link `{id}`"));
            }
            let out = remove_let(src, id);
            edit_list(&out, "links", |names| names.retain(|name| name != id))?
        }
        Op::Header(header) => {
            let mut out = src.to_string();
            let set = |out: &mut String, key: &str, value: Option<String>| -> Result<(), String> {
                *out = set_root_field(out, key, value.as_deref())?;
                Ok(())
            };
            if let Some(title) = &header.title {
                set(&mut out, "title", Some(string_literal(title)))?;
            }
            if let Some(duration) = header.duration {
                if !(duration > 0.0 && duration.is_finite()) {
                    return Err("`duration` is a positive number of seconds".into());
                }
                set(&mut out, "duration", Some(number(duration)))?;
            }
            if let Some(bpm) = header.bpm {
                if bpm.is_some_and(|bpm| !(bpm > 0.0 && bpm < 1000.0)) {
                    return Err("`bpm` is a tempo between 0 and 1000".into());
                }
                set(&mut out, "bpm", bpm.map(|bpm| number(bpm).trim_end_matches(".0").to_string()))?;
            }
            if let Some(offset) = header.beat_offset {
                set(&mut out, "beat_offset", (offset != 0.0).then(|| number(offset)))?;
            }
            if let Some(music) = &header.music {
                if let Some(music) = music {
                    if board.item(music).is_none() {
                        return Err(format!("`music` names an item (a Ref on the music row); there is no `{music}`"));
                    }
                }
                set(&mut out, "music", music.clone())?;
            }
            for (key, value) in [("brief", &header.brief), ("style", &header.style), ("kit", &header.kit)] {
                if let Some(value) = value {
                    set(&mut out, key, (!value.is_empty()).then(|| string_literal(value)))?;
                }
            }
            if let Some((width, height)) = header.format {
                set(&mut out, "format", Some(format!("{{width: {width} height: {height}}}")))?;
            }
            if let Some(edit) = &header.edit {
                set(&mut out, "cut", edit.as_ref().map(|edit| string_literal(edit)))?;
            }
            if let Some(layout) = &header.layout {
                if let Some(layout) = layout {
                    let frame = crate::frame::template(layout).ok_or_else(|| format!("`layout` is one of {}", crate::frame::TEMPLATES.iter().map(|t| t.id).collect::<Vec<_>>().join(", ")))?;
                    // The frame decides the output size too.
                    if header.format.is_none() {
                        set(&mut out, "format", Some(format!("{{width: {} height: {}}}", frame.width, frame.height)))?;
                    }
                }
                set(&mut out, "layout", layout.as_ref().map(|layout| format!("@{layout}")))?;
            }
            out
        }
        Op::Built(built) => set_root_field(src, "built", built.as_ref().map(built_source).as_deref())?,
        Op::Voices(voices) => {
            let mut seen: Vec<&str> = Vec::new();
            for voice in voices {
                if !splash::valid_name(&voice.id) || seen.contains(&voice.id.as_str()) {
                    return Err(format!("`{}` is not a usable voice id (letters, digits and `_`, unique)", voice.id));
                }
                if board.item(&voice.id).is_some() || board.row(&voice.id).is_some() || board.links.iter().any(|link| link.id == voice.id) {
                    return Err(format!("`{}` is taken by an item, row or link: pick another voice id", voice.id));
                }
                seen.push(&voice.id);
            }
            let mut out = src.to_string();
            for voice in &board.voices {
                out = remove_let(&out, &voice.id);
            }
            for voice in voices {
                out = insert_let(&out, &voice.id, &voice_source(voice), &["Row", "Scene", "Note", "Ref", "Link", "Chapter", "Treatment", "Page", "Lyric", "Device", "Voice"])?;
            }
            if voices.is_empty() {
                set_root_field(&out, "voices", None)?
            } else {
                let parsed = parse(&out);
                let root = root_of(&parsed).ok_or("the board has no final `Storyboard{…}`")?;
                let names: Vec<String> = voices.iter().map(|voice| voice.id.clone()).collect();
                let indent = root.value.field("rows").map_or("    ".to_string(), |node| splash::indent_of(&out, node.start));
                splash::set_field(&out, &root.value, "voices", &splash::names_array(&names, &indent))
            }
        }
    };
    let (board, _) = Storyboard::load(&out).map_err(|diags| format!("the change would break the board (a bug; nothing was changed): {}", diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")))?;
    Ok(Applied { source: out, board, changed })
}

/// Apply several ops in order; the first failure stops with nothing applied.
pub fn apply_all(src: &str, ops: &[Op]) -> Result<Applied, String> {
    let mut source = src.to_string();
    let mut changed = Vec::new();
    let mut board = None;
    for op in ops {
        let applied = apply(&source, op)?;
        source = applied.source;
        changed.extend(applied.changed);
        board = Some(applied.board);
    }
    let board = match board {
        Some(board) => board,
        None => Storyboard::load(&source).map_err(|_| "the board does not load".to_string())?.0,
    };
    changed.dedup();
    Ok(Applied { source, board, changed })
}

fn row_names(board: &Storyboard) -> String {
    board.rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>().join(", ")
}

fn item_names(board: &Storyboard) -> String {
    let names: Vec<&str> = board.items.iter().map(|item| item.id.as_str()).collect();
    if names.is_empty() { "none yet".into() } else { names.join(", ") }
}

fn check_refs(board: &Storyboard, item: &Item) -> Result<(), String> {
    if let Some(band) = &item.band {
        check_value(board, item, "band", &Value::Tag(band.clone()))?;
    }
    if let Body::Lyric(lyric) = &item.body {
        if let Some(voice) = &lyric.voice {
            check_value(board, item, "voice", &Value::Tag(voice.clone()))?;
        }
    }
    if let Body::Scene(scene) = &item.body {
        if let Some(from) = &scene.gen.from {
            check_from(board, from)?;
        }
    }
    Ok(())
}

fn check_from(board: &Storyboard, from: &str) -> Result<(), String> {
    match board.item(from).map(|item| &item.body) {
        Some(Body::Ref(reference)) if reference.media.is_some_and(Media::visual) || reference.src.is_some() => Ok(()),
        Some(_) => Err(format!("`from: {from}` must name a picture or video reference")),
        None => Err(format!("`from: {from}` names no item")),
    }
}

fn check_value(board: &Storyboard, item: &Item, field: &str, value: &Value) -> Result<(), String> {
    let bad = |what: &str| Err(format!("`{field}` takes {what}"));
    match (field, value) {
        (_, Value::Clear) if matches!(field, "row" | "span") => bad("a value; it cannot be removed"),
        (_, Value::Clear) => Ok(()),
        ("row", Value::Name(row)) => if board.row(row).is_some() { Ok(()) } else { Err(format!("there is no row `{row}` (rows: {})", row_names(board))) },
        ("row", _) => bad("a row id"),
        ("span", _) => bad("[start, length]: use storyboard_move"),
        ("from", Value::Tag(from) | Value::Name(from)) => check_from(board, from),
        ("from", _) => bad("the id of a reference item"),
        ("media", Value::Tag(tag)) => if Media::from_tag(tag).is_some() { Ok(()) } else { bad(&format!("one of {}", Media::ALL.map(Media::tag).join(", "))) },
        ("role", Value::Tag(tag)) => if Role::from_tag(tag).is_some() { Ok(()) } else { bad("reference, use or generate") },
        ("band", Value::Tag(band) | Value::Name(band)) => match board.frame() {
            Some(frame) if frame.band(band).is_some() => Ok(()),
            Some(frame) => Err(format!("`band` is one of the `{}` layout's bands: {}", frame.id, frame.band_ids().join(", "))),
            None => Err("the board has no frame layout: set one first (storyboard_set layout: shorts)".into()),
        },
        ("media" | "role" | "band", _) => bad("a name"),
        ("src", Value::Asset(_)) => Ok(()),
        ("src", _) => bad("{asset_id, revision}"),
        ("at", Value::Num(_)) => Ok(()),
        ("seed", Value::Int(_)) => Ok(()),
        ("at" | "seed", _) => bad("a number"),
        ("chosen", Value::Bool(_)) => Ok(()),
        ("chosen", _) => bad("true or false"),
        ("palette", Value::Colors(_)) => Ok(()),
        ("palette", _) => bad("colours `#rrggbb`"),
        ("refs", Value::Tags(ids)) => match ids.iter().find(|id| board.item(id).is_none()) {
            Some(id) => Err(format!("`refs` names `{id}`, which is not an item")),
            None => Ok(()),
        },
        ("refs", _) => bad("item ids"),
        ("voice", Value::Tag(voice) | Value::Name(voice)) => match board.voice(voice) {
            Some(_) => Ok(()),
            None if board.voices.is_empty() => Err("the board has no type voices: set them first (storyboard_set voices: \"lyric_film\" or a list)".into()),
            None => Err(format!("`voice` is one of the board's voices: {}", board.voices.iter().map(|v| v.id.as_str()).collect::<Vec<_>>().join(", "))),
        },
        ("voice", _) => bad("a voice id"),
        ("color", Value::Color(_)) => Ok(()),
        ("colors", Value::Colors(_)) => Ok(()),
        ("color" | "colors", _) => bad("colours `#rrggbb`"),
        (_, Value::Text(_)) => {
            let _ = item;
            Ok(())
        }
        _ => bad("text"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) const BOARD: &str = "use mod.storyboard.*\n\n// The rows\nlet story = Row{kind: @story label: \"Story\"}\nlet voice = Row{kind: @voice label: \"Voiceover\"}\nlet refs = Row{kind: @refs label: \"References\"}\n\nlet hook = Scene{row: story span: [0.0, 4.0] title: \"Hook\" intent: \"one tap\"}\n// the line\nlet vo1 = Note{row: voice span: [0.5, 3.0] text: \"Your ideas move fast.\"}\nlet photo = Ref{row: refs span: [0.0, 4.0] media: @image src: asset(\"ast_a\", \"arev_b\") label: \"Desk\"}\n\nStoryboard{\n    title: \"Notes launch\"\n    duration: 20.0\n    bpm: 120\n    rows: [story, voice, refs]\n    items: [hook, vo1, photo]\n}\n";

    fn note(row: &str, start: f64, length: f64, text: &str) -> Item {
        Item { id: String::new(), row: row.into(), start, length, color: None, band: None, body: Body::Note { text: text.into() } }
    }

    #[test]
    fn add_update_move_delete_keep_the_rest_of_the_source() {
        let added = apply(BOARD, &Op::Add(note("voice", 4.0, 3.0, "Now they stay."))).unwrap();
        assert_eq!(added.changed, vec!["note1".to_string()]);
        assert!(added.source.contains("label: \"Desk\"}\nlet note1 = Note{row: voice span: [4.0, 3.0] text: \"Now they stay.\"}\n"), "{}", added.source);
        assert!(added.source.contains("items: [hook, vo1, photo, note1]"));
        assert!(added.source.contains("// The rows\n") && added.source.contains("// the line\n"));
        let updated = apply(&added.source, &Op::Update { id: "hook".into(), patch: Patch { fields: vec![("mood".into(), Value::Text("bright".into())), ("intent".into(), Value::Text("two taps".into()))] } }).unwrap();
        assert!(updated.source.contains("let hook = Scene{row: story span: [0.0, 4.0] title: \"Hook\" intent: \"two taps\" mood: \"bright\"}"), "{}", updated.source);
        let placed = apply(&updated.source, &Op::Place(vec![("note1".into(), 5.0, 2.5, None)])).unwrap();
        assert!(placed.source.contains("let note1 = Note{row: voice span: [5.0, 2.5]"));
        assert_eq!(placed.board.item("note1").unwrap().start, 5.0);
        let deleted = apply(&placed.source, &Op::Delete(vec!["note1".into()])).unwrap();
        assert!(!deleted.source.contains("note1"));
        // Back to where it was, but for the update.
        assert_eq!(deleted.source, updated.source.replace("let note1 = Note{row: voice span: [4.0, 3.0] text: \"Now they stay.\"}\n", "").replace("items: [hook, vo1, photo, note1]", "items: [hook, vo1, photo]"));
    }

    #[test]
    fn bad_calls_say_how_to_fix_them() {
        assert!(apply(BOARD, &Op::Add(note("nope", 0.0, 1.0, "x"))).unwrap_err().contains("rows: story, voice, refs"));
        assert!(apply(BOARD, &Op::Add(note("voice", 0.0, 0.0, "x"))).unwrap_err().contains("length > 0"));
        assert!(apply(BOARD, &Op::Update { id: "vo1".into(), patch: Patch { fields: vec![("title".into(), Value::Text("x".into()))] } }).unwrap_err().contains("has no field `title`"));
        assert!(apply(BOARD, &Op::Update { id: "hook".into(), patch: Patch { fields: vec![("from".into(), Value::Tag("vo1".into()))] } }).unwrap_err().contains("picture"));
        let mut taken = note("voice", 0.0, 1.0, "x");
        taken.id = "hook".into();
        assert!(apply(BOARD, &Op::Add(taken)).unwrap_err().contains("taken"));
    }

    #[test]
    fn links_rows_and_header() {
        let linked = apply(BOARD, &Op::Link(Link { id: String::new(), from: "hook".into(), to: "photo".into(), label: "style".into() })).unwrap();
        assert!(linked.source.contains("let link1 = Link{from: hook to: photo label: \"style\"}"));
        assert!(linked.source.contains("links: [link1]"));
        assert_eq!(linked.board.links.len(), 1);
        // Deleting an item drops its links.
        let deleted = apply(&linked.source, &Op::Delete(vec!["photo".into()])).unwrap();
        assert!(deleted.board.links.is_empty() && !deleted.source.contains("link1"));
        let row = apply(BOARD, &Op::AddRow(Row { id: String::new(), kind: RowKind::Music, label: "Music".into() })).unwrap();
        assert!(row.source.contains("let refs = Row{kind: @refs label: \"References\"}\nlet music = Row{kind: @music label: \"Music\"}\n"), "{}", row.source);
        assert_eq!(row.board.rows.last().unwrap().id, "music");
        let moved = apply(&row.source, &Op::MoveRow { id: "music".into(), index: 0 }).unwrap();
        assert_eq!(moved.board.rows[0].id, "music");
        let gone = apply(BOARD, &Op::DeleteRow("voice".into())).unwrap();
        assert!(gone.board.item("vo1").is_none() && gone.board.row("voice").is_none());
        let header = apply(BOARD, &Op::Header(Header { duration: Some(24.0), bpm: Some(None), style: Some("warm".into()), ..Default::default() })).unwrap();
        assert!(header.source.contains("duration: 24.0") && !header.source.contains("bpm") && header.source.contains("style: \"warm\""), "{}", header.source);
        assert_eq!(header.board.bpm, None);
    }
}

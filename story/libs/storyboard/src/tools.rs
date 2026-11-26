//! The storyboard tools of the AI control API (`storyboard_*` in
//! `makepad_studio_api::catalog`): the pure part. Stage's host
//! keeps the board (its source, undo and autosave) and calls [`call`]
//! for the tools that only read or change the board; the tools that touch
//! the cut or the library (`storyboard_new`, `…_plan_update`,
//! `…_apply_changes`, `…_mark_built`, `…_look`) are the host's, built on
//! [`crate::build`].

use crate::diff::{describe, diff};
use crate::edit::{apply, apply_all, Applied, Header, Op, Patch, Value};
use crate::model::*;
use crate::splash;
use makepad_strict_json::{self as json, Value as Json};

/// What a pure tool call did.
pub struct Called {
    /// The answer's JSON.
    pub json: Json,
    /// A change: the new source (one undo step).
    pub source: Option<String>,
}

fn num(value: f64) -> Json {
    Json::F64((value * 1000.0).round() / 1000.0)
}

fn text(value: &str) -> Json {
    json::s(value)
}

/// An item as the tools show it.
pub fn item_json(board: &Storyboard, item: &Item) -> Json {
    let mut pairs = vec![
        ("id", text(&item.id)),
        ("kind", text(item.body.kind())),
        ("row", text(&item.row)),
        ("start", num(item.start)),
        ("length", num(item.length)),
        ("end", num(item.end())),
    ];
    if let Some(n) = board.scene_number(&item.id) {
        pairs.push(("scene", Json::Int(n as i64)));
    }
    if let Some(n) = board.chapter_number(&item.id) {
        pairs.push(("chapter", text(&format!("{n:02}/{:02}", board.chapters().len()))));
    }
    if let Some(color) = item.color {
        pairs.push(("color", text(&splash::color_literal(color))));
    }
    if let Some(band) = &item.band {
        pairs.push(("band", text(band)));
    }
    if board.bpm.is_some() && matches!(item.body, Body::Chapter(_) | Body::Scene(_)) {
        pairs.push(("on_beat", Json::Bool(board.on_beat(item.start) && board.on_beat(item.end()))));
    }
    let put = |pairs: &mut Vec<(&str, Json)>, key: &'static str, value: &str| {
        if !value.is_empty() {
            pairs.push((key, text(value)));
        }
    };
    match &item.body {
        Body::Note { text: words } => pairs.push(("text", text(words))),
        Body::Treatment(treatment) => {
            pairs.push(("title", text(&treatment.title)));
            put(&mut pairs, "idea", &treatment.idea);
            put(&mut pairs, "look", &treatment.look);
            if !treatment.palette.is_empty() {
                pairs.push(("palette", Json::Arr(treatment.palette.iter().map(|c| text(&splash::color_literal(*c))).collect())));
            }
            pairs.push(("chosen", Json::Bool(treatment.chosen)));
        }
        Body::Lyric(lyric) => {
            pairs.push(("text", text(&lyric.text)));
            put(&mut pairs, "pun", &lyric.pun);
            put(&mut pairs, "set", &lyric.set);
            if let Some(voice) = &lyric.voice {
                pairs.push(("voice", text(voice)));
            }
            put(&mut pairs, "device", &lyric.device);
        }
        Body::Device(device) => {
            pairs.push(("name", text(&device.name)));
            pairs.push(("value", text(&device.value)));
            put(&mut pairs, "note", &device.note);
        }
        Body::Page(page) => {
            pairs.push(("title", text(&page.title)));
            put(&mut pairs, "body", &page.body);
            if !page.refs.is_empty() {
                pairs.push(("refs", Json::Arr(page.refs.iter().map(|id| text(id)).collect())));
            }
        }
        Body::Chapter(chapter) => {
            pairs.push(("skill", text(&chapter.skill)));
            for (key, value) in [("look", &chapter.look), ("hero", &chapter.hero), ("out", &chapter.out)] {
                put(&mut pairs, key, value);
            }
        }
        Body::Scene(scene) => {
            pairs.push(("title", text(&scene.title)));
            for (key, value) in [("intent", &scene.intent), ("mood", &scene.mood), ("look", &scene.look), ("kit", &scene.kit), ("music", &scene.music), ("media", &scene.media), ("stack", &scene.stack), ("bars", &scene.bars), ("camera", &scene.camera), ("why", &scene.why), ("lyric", &scene.lyric)] {
                put(&mut pairs, key, value);
            }
            gen_json(&mut pairs, &scene.gen, true);
        }
        Body::Ref(reference) => {
            if let Some(media) = reference.media {
                pairs.push(("media_type", text(media.tag())));
            }
            if let Some(src) = &reference.src {
                pairs.push(("asset", json::obj(vec![("asset_id", text(&src.asset_id)), ("revision", text(&src.revision))])));
            }
            if let Some(at) = reference.at {
                pairs.push(("at", num(at)));
            }
            put(&mut pairs, "url", &reference.url);
            if !reference.colors.is_empty() {
                pairs.push(("colors", Json::Arr(reference.colors.iter().map(|c| text(&splash::color_literal(*c))).collect())));
            }
            put(&mut pairs, "font", &reference.font);
            put(&mut pairs, "file", &reference.file);
            put(&mut pairs, "label", &reference.label);
            put(&mut pairs, "note", &reference.note);
            pairs.push(("role", text(reference.role.tag())));
            gen_json(&mut pairs, &reference.gen, false);
        }
    }
    let links: Vec<Json> = board.links.iter().filter_map(|link| {
        if link.from == item.id {
            Some(text(&link.to))
        } else if link.to == item.id {
            Some(text(&link.from))
        } else {
            None
        }
    }).collect();
    if !links.is_empty() {
        pairs.push(("linked", Json::Arr(links)));
    }
    json::obj(pairs)
}

fn gen_json(pairs: &mut Vec<(&str, Json)>, gen: &Gen, with_from: bool) {
    if with_from {
        if let Some(from) = &gen.from {
            pairs.push(("from", text(from)));
        }
    }
    for (key, value) in [("prompt", &gen.prompt), ("negative", &gen.negative), ("flow", &gen.flow)] {
        if !value.is_empty() {
            pairs.push((key, text(value)));
        }
    }
    if let Some(seed) = gen.seed {
        pairs.push(("seed", Json::Int(seed)));
    }
}

/// A frame layout as the tools show it: its bands and safe zones.
pub fn frame_json(frame: &crate::frame::Template) -> Json {
    let f = |value: f64| Json::F64((value * 1000.0).round() / 1000.0);
    json::obj(vec![
        ("id", text(frame.id)),
        ("label", text(frame.label)),
        ("size", json::obj(vec![("width", Json::Int(frame.width as i64)), ("height", Json::Int(frame.height as i64))])),
        ("rule", text(frame.rule)),
        ("safe", json::obj(vec![("top", f(frame.safe.top)), ("bottom", f(frame.safe.bottom)), ("left", f(frame.safe.left)), ("right", f(frame.safe.right)), ("covers", text(frame.safe_what))])),
        ("bands", Json::Arr(frame.bands.iter().map(|band| json::obj(vec![
            ("id", text(band.id)),
            ("label", text(band.label)),
            ("rect", Json::Arr(vec![f(band.x0), f(band.y0), f(band.x1), f(band.y1)])),
            ("text", Json::Bool(band.text)),
            ("what", text(band.what)),
        ])).collect())),
    ])
}

/// The whole board as `storyboard_read` answers.
pub fn board_json(board: &Storyboard, source: Option<&str>) -> Json {
    let mut items: Vec<&Item> = board.items.iter().collect();
    items.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| board.rows.iter().position(|r| r.id == a.row).cmp(&board.rows.iter().position(|r| r.id == b.row))));
    let (stale, unbuilt) = crate::build::stale(board);
    let mut pairs = vec![
        ("title", text(&board.title)),
        ("duration", num(board.duration)),
        ("revision", text(&board.revision())),
    ];
    if let Some(bpm) = board.bpm {
        pairs.push(("bpm", num(bpm)));
        pairs.push(("beat_offset", num(board.beat_offset)));
        pairs.push(("beats_per_bar", Json::Int(board.beats_per_bar as i64)));
    }
    if let Some(music) = &board.music {
        pairs.push(("music", text(music)));
    }
    for (key, value) in [("brief", &board.brief), ("style", &board.style), ("kit", &board.kit)] {
        if !value.is_empty() {
            pairs.push((key, text(value)));
        }
    }
    if let Some(edit) = &board.edit {
        pairs.push(("cut", text(edit)));
    }
    if !board.voices.is_empty() {
        pairs.push(("voices", Json::Arr(board.voices.iter().map(|voice| {
            let mut fields = vec![("id", text(&voice.id)), ("label", text(&voice.label)), ("face", text(&voice.face)), ("role", text(&voice.role))];
            if let Some(color) = voice.color {
                fields.push(("color", text(&splash::color_literal(color))));
            }
            json::obj(fields)
        }).collect())));
    }
    let devices = board.devices();
    if !devices.is_empty() {
        pairs.push(("devices", Json::Arr(devices.iter().map(|(name, list)| json::obj(vec![
            ("name", text(name)),
            ("occurrences", Json::Arr(list.iter().map(|(id, start, value)| json::obj(vec![("id", text(id)), ("start", num(*start)), ("value", text(value))])).collect())),
        ])).collect())));
    }
    if let Some(frame) = board.frame() {
        pairs.push(("layout", frame_json(frame)));
    }
    if let Some((width, height)) = board.format {
        pairs.push(("format", json::obj(vec![("width", Json::Int(width as i64)), ("height", Json::Int(height as i64))])));
    }
    pairs.push(("rows", Json::Arr(board.rows.iter().map(|row| json::obj(vec![("id", text(&row.id)), ("kind", text(row.kind.tag())), ("label", text(&row.label))])).collect())));
    pairs.push(("items", Json::Arr(items.iter().map(|item| item_json(board, item)).collect())));
    if !board.links.is_empty() {
        pairs.push(("links", Json::Arr(board.links.iter().map(|link| json::obj(vec![("id", text(&link.id)), ("from", text(&link.from)), ("to", text(&link.to)), ("label", text(&link.label))])).collect())));
    }
    match &board.built {
        Some(built) => pairs.push(("built", json::obj(vec![
            ("cut", json::obj(vec![("asset_id", text(&built.edit.asset_id)), ("revision", text(&built.edit.revision))])),
            ("parts", Json::Arr(built.parts.iter().map(|part| json::obj(vec![("item", text(&part.item)), ("nodes", Json::Arr(part.nodes.iter().map(|n| text(n)).collect()))])).collect())),
            ("out_of_date", Json::Arr(stale.iter().map(|id| text(id)).collect())),
            ("not_built", Json::Arr(unbuilt.iter().map(|id| text(id)).collect())),
        ]))),
        None => pairs.push(("built", Json::Null)),
    }
    if let Some(source) = source {
        pairs.push(("source", text(source)));
    }
    json::obj(pairs)
}

fn arg_str<'a>(args: &'a Json, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Json::as_str)
}

fn arg_num(args: &Json, key: &str) -> Option<f64> {
    match args.get(key) {
        Some(Json::F64(n)) => Some(*n),
        Some(Json::Int(n)) => Some(*n as f64),
        _ => None,
    }
}

fn color_arg(text: &str) -> Result<u32, String> {
    splash::parse_color(text.trim().trim_start_matches('#')).ok_or_else(|| format!("`{text}` is not a colour: write #rrggbb"))
}

/// Tool field → board field (the tools say `asset` and `media_type` where
/// the source says `src` and `media`).
fn source_field(key: &str) -> &str {
    match key {
        "asset" => "src",
        "media_type" => "media",
        other => other,
    }
}

/// The patch for the item fields in `args` (all keys but `except`).
pub fn patch_of(args: &Json, except: &[&str]) -> Result<Patch, String> {
    let Json::Obj(pairs) = args else { return Ok(Patch::default()) };
    let mut patch = Patch::default();
    for (key, value) in pairs {
        if except.contains(&key.as_str()) {
            continue;
        }
        let field = source_field(key).to_string();
        let parsed = match (field.as_str(), value) {
            (_, Json::Null) => Value::Clear,
            ("from", Json::Str(name)) if name.is_empty() => Value::Clear,
            ("from", Json::Str(name)) => Value::Tag(name.trim_start_matches('@').to_string()),
            ("row", Json::Str(name)) => Value::Name(name.clone()),
            ("band", Json::Str(band)) if band.is_empty() => Value::Clear,
            ("media" | "role" | "band", Json::Str(tag)) => Value::Tag(tag.trim_start_matches('@').to_string()),
            ("src", Json::Obj(_)) => {
                let asset_id = value.get("asset_id").and_then(Json::as_str).unwrap_or("");
                let revision = value.get("revision").and_then(Json::as_str).unwrap_or("");
                if asset_id.is_empty() || revision.is_empty() {
                    return Err("`asset` is {\"asset_id\": \"…\", \"revision\": \"…\"}".into());
                }
                Value::Asset(AssetRef { asset_id: asset_id.into(), revision: revision.into() })
            }
            ("at", Json::F64(n)) => Value::Num(*n),
            ("at", Json::Int(n)) => Value::Num(*n as f64),
            ("seed", Json::Int(n)) => Value::Int(*n),
            ("color", Json::Str(color)) if color.is_empty() => Value::Clear,
            ("color", Json::Str(color)) => Value::Color(color_arg(color)?),
            ("chosen", Json::Bool(chosen)) => Value::Bool(*chosen),
            ("voice", Json::Str(voice)) if voice.is_empty() => Value::Clear,
            ("voice", Json::Str(voice)) => Value::Tag(voice.trim_start_matches('@').to_string()),
            ("refs", Json::Arr(ids)) => Value::Tags(ids.iter().map(|id| id.as_str().map(|id| id.trim_start_matches('@').to_string()).ok_or_else(|| "`refs` holds item ids".to_string())).collect::<Result<_, _>>()?),
            ("colors" | "palette", Json::Arr(colors)) => Value::Colors(colors.iter().map(|c| c.as_str().ok_or_else(|| "`colors` holds \"#rrggbb\" strings".to_string()).and_then(color_arg)).collect::<Result<_, _>>()?),
            (_, Json::Str(words)) => Value::Text(words.clone()),
            (key, other) => return Err(format!("`{key}` does not take {}", other.to_json())),
        };
        patch.fields.push((field, parsed));
    }
    Ok(patch)
}

/// A new item from `storyboard_add`'s entry.
pub fn item_of(board: &Storyboard, entry: &Json) -> Result<Item, String> {
    let kind = arg_str(entry, "kind").ok_or("each item needs `kind`: scene, chapter, lyric, device, note, ref, treatment or page")?;
    let start = arg_num(entry, "start").ok_or("each item needs `start` in seconds")?;
    let length = arg_num(entry, "length").ok_or("each item needs `length` in seconds")?;
    let (body, default_row) = match kind {
        "scene" => (Body::Scene(Scene::default()), RowKind::Story),
        "note" => (Body::Note { text: String::new() }, RowKind::Notes),
        "ref" => (Body::Ref(Reference::default()), RowKind::Refs),
        "chapter" => (Body::Chapter(crate::Chapter::default()), RowKind::Chapters),
        "treatment" => (Body::Treatment(crate::Treatment::default()), RowKind::Treatments),
        "page" => (Body::Page(crate::Page::default()), RowKind::Bible),
        "lyric" => (Body::Lyric(crate::Lyric::default()), RowKind::Lyrics),
        "device" => (Body::Device(crate::Device::default()), RowKind::Devices),
        other => return Err(format!("`kind: {other}` is scene, chapter, lyric, device, note, ref, treatment or page")),
    };
    let row = match arg_str(entry, "row") {
        Some(row) => row.to_string(),
        None => board.row_of(default_row).or_else(|| board.rows.first()).map(|row| row.id.clone()).ok_or("the board has no rows: add one with storyboard_row")?,
    };
    let mut item = Item { id: arg_str(entry, "id").unwrap_or("").to_string(), row, start, length, color: None, band: None, body };
    // The fields go through the same checks as an update, on the new item.
    let patch = patch_of(entry, &["kind", "id", "start", "length", "row"])?;
    let ty = item.body.ty();
    for (field, value) in patch.fields {
        if !crate::edit::fields_of(ty).contains(&field.as_str()) {
            return Err(format!("a {} has no field `{field}`; it has {}", item.body.kind(), crate::edit::fields_of(ty).iter().filter(|f| !matches!(**f, "row" | "span")).copied().collect::<Vec<_>>().join(", ")));
        }
        set_on(&mut item, &field, value)?;
    }
    Ok(item)
}

fn set_on(item: &mut Item, field: &str, value: Value) -> Result<(), String> {
    let text = |value: &Value| match value {
        Value::Text(text) => text.clone(),
        _ => String::new(),
    };
    if field == "color" {
        item.color = match value {
            Value::Color(color) => Some(color),
            _ => None,
        };
        return Ok(());
    }
    if field == "band" {
        item.band = match value {
            Value::Tag(band) | Value::Name(band) => Some(band),
            _ => None,
        };
        return Ok(());
    }
    let gen_set = |gen: &mut Gen, field: &str, value: &Value| {
        match field {
            "prompt" => gen.prompt = text(value),
            "negative" => gen.negative = text(value),
            "flow" => gen.flow = text(value),
            "seed" => gen.seed = match value {
                Value::Int(n) => Some(*n),
                _ => None,
            },
            "from" => gen.from = match value {
                Value::Tag(name) | Value::Name(name) => Some(name.clone()),
                _ => None,
            },
            _ => return false,
        }
        true
    };
    match &mut item.body {
        Body::Note { text: words } => {
            if field == "text" {
                *words = text(&value);
            }
        }
        Body::Treatment(treatment) => match (field, value) {
            ("chosen", Value::Bool(chosen)) => treatment.chosen = chosen,
            ("palette", Value::Colors(colors)) => treatment.palette = colors,
            ("title", value) => treatment.title = text(&value),
            ("idea", value) => treatment.idea = text(&value),
            ("look", value) => treatment.look = text(&value),
            _ => {}
        },
        Body::Lyric(lyric) => match (field, value) {
            ("voice", Value::Tag(voice) | Value::Name(voice)) => lyric.voice = Some(voice),
            ("voice", _) => lyric.voice = None,
            ("text", value) => lyric.text = text(&value),
            ("pun", value) => lyric.pun = text(&value),
            ("set", value) => lyric.set = text(&value),
            ("device", value) => lyric.device = text(&value),
            _ => {}
        },
        Body::Device(device) => match (field, value) {
            ("name", value) => device.name = text(&value),
            ("value", value) => device.value = text(&value),
            ("note", value) => device.note = text(&value),
            _ => {}
        },
        Body::Page(page) => match (field, value) {
            ("refs", Value::Tags(ids)) => page.refs = ids,
            ("title", value) => page.title = text(&value),
            ("body", value) => page.body = text(&value),
            _ => {}
        },
        Body::Chapter(chapter) => {
            let slot = match field {
                "skill" => &mut chapter.skill,
                "look" => &mut chapter.look,
                "hero" => &mut chapter.hero,
                _ => &mut chapter.out,
            };
            *slot = text(&value);
        }
        Body::Scene(scene) => {
            if !gen_set(&mut scene.gen, field, &value) {
                let slot = match field {
                    "title" => &mut scene.title,
                    "intent" => &mut scene.intent,
                    "mood" => &mut scene.mood,
                    "look" => &mut scene.look,
                    "kit" => &mut scene.kit,
                    "music" => &mut scene.music,
                    "media" => &mut scene.media,
                    "bars" => &mut scene.bars,
                    "camera" => &mut scene.camera,
                    "why" => &mut scene.why,
                    "lyric" => &mut scene.lyric,
                    _ => &mut scene.stack,
                };
                *slot = text(&value);
            }
        }
        Body::Ref(reference) => {
            if !gen_set(&mut reference.gen, field, &value) {
                match (field, value) {
                    ("media", Value::Tag(tag)) => reference.media = Some(Media::from_tag(&tag).ok_or_else(|| format!("`media_type: {tag}` is one of {}", Media::ALL.map(Media::tag).join(", ")))?),
                    ("role", Value::Tag(tag)) => reference.role = Role::from_tag(&tag).ok_or("`role` is reference, use or generate")?,
                    ("src", Value::Asset(asset)) => reference.src = Some(asset),
                    ("at", Value::Num(at)) => reference.at = Some(at),
                    ("colors", Value::Colors(colors)) => reference.colors = colors,
                    ("url", value) => reference.url = text(&value),
                    ("font", value) => reference.font = text(&value),
                    ("file", value) => reference.file = text(&value),
                    ("label", value) => reference.label = text(&value),
                    ("note", value) => reference.note = text(&value),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

fn changed_json(before: &Storyboard, applied: &Applied) -> Json {
    let changes = diff(before, &applied.board);
    let (stale, _) = crate::build::stale(&applied.board);
    json::obj(vec![
        ("revision", text(&applied.board.revision())),
        ("changed", Json::Arr(applied.changed.iter().map(|id| text(id)).collect())),
        ("changes", text(&describe(before, &applied.board, &changes))),
        ("stale", Json::Arr(stale.iter().map(|id| text(id)).collect())),
    ])
}

/// Run a pure storyboard tool on the board in `source`. `None` for a tool
/// this module does not run (the host's).
pub fn call(name: &str, args: &Json, source: &str) -> Option<Result<Called, String>> {
    Some(run(name, args, source)?.map_err(|error| error.to_string()))
}

fn run(name: &str, args: &Json, source: &str) -> Option<Result<Called, String>> {
    let loaded = Storyboard::load(source).map(|(board, _)| board).map_err(|diags| format!("the storyboard does not load ({}): fix it with storyboard_write", diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ")));
    let board = match loaded {
        Ok(board) => board,
        Err(error) => return Some(Err(error)),
    };
    let change = |ops: Vec<Op>| -> Result<Called, String> {
        let applied = apply_all(source, &ops)?;
        Ok(Called { json: changed_json(&board, &applied), source: Some(applied.source) })
    };
    Some(match name {
        "storyboard_read" => Ok(Called { json: board_json(&board, args.get("source").and_then(Json::as_bool).unwrap_or(false).then_some(source)), source: None }),
        "storyboard_add" => {
            let entries = args.get("items").and_then(Json::as_arr).unwrap_or(&[]);
            let mut ops = Vec::new();
            let mut scratch = board.clone();
            for (index, entry) in entries.iter().enumerate() {
                match item_of(&scratch, entry) {
                    Ok(mut item) => {
                        if item.id.is_empty() {
                            item.id = scratch.fresh_id(item.body.kind());
                        }
                        scratch.items.push(item.clone());
                        ops.push(Op::Add(item));
                    }
                    Err(error) => return Some(Err(format!("item {}: {error}", index + 1))),
                }
            }
            change(ops)
        }
        "storyboard_update" => {
            let Some(id) = arg_str(args, "id") else { return Some(Err("`id` is missing".into())) };
            match patch_of(args, &["id"]) {
                Ok(patch) if patch.fields.is_empty() => Err("give at least one field to change".into()),
                Ok(patch) => {
                    // One treatment is chosen at a time (per row).
                    let mut ops = vec![Op::Update { id: id.into(), patch: patch.clone() }];
                    if patch.fields.iter().any(|(field, value)| field == "chosen" && *value == Value::Bool(true)) {
                        let row = board.item(id).map(|item| item.row.clone()).unwrap_or_default();
                        for other in board.items.iter().filter(|other| other.id != id && other.row == row && matches!(&other.body, Body::Treatment(t) if t.chosen)) {
                            ops.push(Op::Update { id: other.id.clone(), patch: Patch { fields: vec![("chosen".into(), Value::Bool(false))] } });
                        }
                    }
                    change(ops)
                }
                Err(error) => Err(error),
            }
        }
        "storyboard_move" => {
            let mut places = Vec::new();
            for entry in args.get("moves").and_then(Json::as_arr).unwrap_or(&[]) {
                let Some(id) = arg_str(entry, "id") else { return Some(Err("each move needs `id`".into())) };
                let Some(item) = board.item(id) else { return Some(Err(format!("there is no item `{id}`"))) };
                places.push((id.to_string(), arg_num(entry, "start").unwrap_or(item.start), arg_num(entry, "length").unwrap_or(item.length), arg_str(entry, "row").map(str::to_string)));
            }
            change(vec![Op::Place(places)])
        }
        "storyboard_delete" => {
            let ids: Vec<String> = args.get("ids").and_then(Json::as_arr).unwrap_or(&[]).iter().filter_map(|id| id.as_str().map(str::to_string)).collect();
            let mut ops = Vec::new();
            if args.get("close_gap").and_then(Json::as_bool) == Some(true) {
                ops.extend(close_gap_ops(&board, &ids));
            }
            ops.insert(0, Op::Delete(ids));
            change(ops)
        }
        "storyboard_row" => {
            let id = arg_str(args, "id").unwrap_or("").to_string();
            let op = match arg_str(args, "action").unwrap_or("") {
                "add" => {
                    let kind = match arg_str(args, "kind").map(RowKind::from_tag) {
                        Some(Some(kind)) => kind,
                        _ => return Some(Err("add a row with `kind`: story, visuals, voice, music, refs or notes".into())),
                    };
                    let label = arg_str(args, "label").map_or_else(|| kind.label().to_string(), str::to_string);
                    Op::AddRow(Row { id, kind, label })
                }
                "delete" => Op::DeleteRow(id),
                "move" => Op::MoveRow { id, index: arg_num(args, "index").unwrap_or(0.0).max(0.0) as usize },
                "rename" => Op::RenameRow { id, label: arg_str(args, "label").unwrap_or("").to_string() },
                other => return Some(Err(format!("`action: {other}` is add, delete, move or rename"))),
            };
            change(vec![op])
        }
        "storyboard_set" => {
            let mut header = Header {
                title: arg_str(args, "title").map(str::to_string),
                duration: arg_num(args, "duration"),
                beat_offset: arg_num(args, "beat_offset"),
                brief: arg_str(args, "brief").map(str::to_string),
                style: arg_str(args, "style").map(str::to_string),
                kit: arg_str(args, "kit").map(str::to_string),
                ..Default::default()
            };
            match args.get("bpm") {
                Some(Json::Null) => header.bpm = Some(None),
                Some(_) => header.bpm = Some(arg_num(args, "bpm")),
                None => {}
            }
            match args.get("music") {
                Some(Json::Null) => header.music = Some(None),
                Some(Json::Str(music)) => header.music = Some(Some(music.clone())),
                _ => {}
            }
            let voices = match args.get("voices") {
                None => None,
                Some(Json::Null) => Some(Vec::new()),
                Some(Json::Str(preset)) if preset == "lyric_film" => Some(crate::model::lyric_film_voices()),
                Some(Json::Str(preset)) => return Some(Err(format!("`voices: \"{preset}\"`: the preset is \"lyric_film\", or give a list of {{id, label, face, role, color}}"))),
                Some(Json::Arr(list)) => {
                    let mut voices = Vec::new();
                    for entry in list {
                        let Some(id) = arg_str(entry, "id") else { return Some(Err("each voice needs `id`".into())) };
                        let color = match arg_str(entry, "color") {
                            Some(color) if !color.is_empty() => match color_arg(color) {
                                Ok(color) => Some(color),
                                Err(error) => return Some(Err(error)),
                            },
                            _ => None,
                        };
                        let get = |key: &str| arg_str(entry, key).unwrap_or("").to_string();
                        voices.push(crate::TypeVoice { id: id.to_string(), label: if get("label").is_empty() { id.to_string() } else { get("label") }, face: get("face"), role: get("role"), color });
                    }
                    Some(voices)
                }
                Some(_) => return Some(Err("`voices` is a list of {id, label, face, role, color}, \"lyric_film\", or null".into())),
            };
            match args.get("layout") {
                Some(Json::Null) => header.layout = Some(None),
                Some(Json::Str(layout)) if layout.is_empty() || layout == "none" => header.layout = Some(None),
                Some(Json::Str(layout)) => header.layout = Some(Some(layout.trim_start_matches('@').to_string())),
                _ => {}
            }
            let mut ops = Vec::new();
            if header != Header::default() {
                ops.push(Op::Header(header));
            }
            if let Some(voices) = voices {
                ops.push(Op::Voices(voices));
            }
            if ops.is_empty() {
                return Some(Err("give at least one field to set".into()));
            }
            change(ops)
        }
        "storyboard_link" => {
            let from = arg_str(args, "from").unwrap_or("").to_string();
            let to = arg_str(args, "to").unwrap_or("").to_string();
            if args.get("remove").and_then(Json::as_bool) == Some(true) {
                let ids: Vec<Op> = board.links.iter().filter(|link| (link.from == from && link.to == to) || (link.from == to && link.to == from)).map(|link| Op::Unlink(link.id.clone())).collect();
                if ids.is_empty() {
                    return Some(Err(format!("`{from}` and `{to}` are not linked")));
                }
                change(ids)
            } else {
                change(vec![Op::Link(Link { id: String::new(), from, to, label: arg_str(args, "label").unwrap_or("").to_string() })])
            }
        }
        "storyboard_write" => {
            let text = arg_str(args, "source").unwrap_or("");
            match Storyboard::load(text) {
                Ok((new, warnings)) => {
                    let changes = diff(&board, &new);
                    let mut json = json::obj(vec![
                        ("revision", self::text(&new.revision())),
                        ("changed", Json::Arr(changes.iter().filter_map(|c| c.item()).map(self::text).collect())),
                        ("changes", self::text(&describe(&board, &new, &changes))),
                    ]);
                    if !warnings.is_empty() {
                        if let Json::Obj(pairs) = &mut json {
                            pairs.push(("warnings".into(), Json::Arr(warnings.iter().map(|w| self::text(&w.to_string())).collect())));
                        }
                    }
                    Ok(Called { json, source: Some(text.to_string()) })
                }
                Err(diags) => Err(format!("the source does not load, nothing was changed:\n{}", diags.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))),
            }
        }
        _ => return None,
    })
}

/// The ops that close the gaps `ids` leave: every item starting at or
/// after a deleted item's end moves left by its length.
pub fn close_gap_ops(board: &Storyboard, ids: &[String]) -> Vec<Op> {
    let gaps: Vec<(f64, f64)> = ids.iter().filter_map(|id| board.item(id)).map(|item| (item.end(), item.length)).collect();
    let places: Vec<(String, f64, f64, Option<String>)> = board.items.iter().filter(|item| !ids.contains(&item.id)).filter_map(|item| {
        let shift: f64 = gaps.iter().filter(|(end, _)| item.start >= end - 1e-6).map(|(_, length)| length).sum();
        (shift > 0.0).then(|| (item.id.clone(), (item.start - shift).max(0.0), item.length, None))
    }).collect();
    if places.is_empty() { Vec::new() } else { vec![Op::Place(places)] }
}

/// A new board's source (`storyboard_new`, and the page's "New board").
pub fn new_board(title: &str, duration: f64, bpm: Option<f64>, brief: &str, style: &str, kit: &str) -> String {
    Storyboard { title: title.to_string(), duration, bpm, brief: brief.to_string(), style: style.to_string(), kit: kit.to_string(), ..Default::default() }.with_default_rows().to_source()
}

/// Apply one op to a source (the page's gestures use it).
pub fn op(source: &str, op: Op) -> Result<Applied, String> {
    apply(source, &op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_studio_api::{find, schema, Registry};

    fn args(text: &str) -> Json {
        json::parse_depth(text.as_bytes(), 16).unwrap()
    }

    fn run_ok(name: &str, a: &str, source: &str) -> Called {
        // Every call passes the registry's schema check first.
        let mut registry: Registry<u8> = Registry::new();
        registry.register(name, 0);
        registry.check(name, args(a)).unwrap_or_else(|error| panic!("{name}: {}", error.message));
        call(name, &args(a), source).unwrap().unwrap_or_else(|error| panic!("{name}: {error}"))
    }

    #[test]
    fn the_tools_are_in_the_table_with_valid_schemas() {
        for name in ["storyboard_read", "storyboard_new", "storyboard_add", "storyboard_update", "storyboard_move", "storyboard_delete", "storyboard_row", "storyboard_set", "storyboard_link", "storyboard_write", "storyboard_changes", "storyboard_plan_update", "storyboard_apply_changes", "storyboard_mark_built", "storyboard_look"] {
            let spec = find(name).unwrap_or_else(|| panic!("{name}"));
            schema::check_schema(&spec.input_schema()).unwrap_or_else(|error| panic!("{name}: {error}"));
            schema::check_schema(&spec.output_schema()).unwrap_or_else(|error| panic!("{name}: {error}"));
        }
    }

    #[test]
    fn an_ai_session_builds_and_revises_a_board() {
        let mut source = new_board("Notes launch", 20.0, Some(120.0), "20 s launch for Notes", "", "@swiss");
        let added = run_ok("storyboard_add", r##"{"items": [
            {"kind": "scene", "id": "hook", "start": 0, "length": 4, "title": "Hook", "intent": "one tap, a note", "mood": "bright", "stack": "plate · type"},
            {"kind": "scene", "start": 4, "length": 6, "title": "Demo"},
            {"kind": "note", "row": "voice", "start": 0.5, "length": 3, "text": "Your ideas move fast."},
            {"kind": "ref", "start": 0, "length": 20, "media_type": "swatch", "colors": ["#ff5c39", "#14171c"], "label": "Brand"},
            {"kind": "ref", "start": 4, "length": 3, "media_type": "image", "asset": {"asset_id": "ast_p", "revision": "arev_p"}, "role": "generate", "prompt": "push in", "seed": 3}
        ]}"##, &source);
        source = added.source.unwrap();
        let changed: Vec<&str> = added.json.get("changed").unwrap().as_arr().unwrap().iter().filter_map(Json::as_str).collect();
        assert_eq!(changed, ["hook", "scene1", "note1", "ref1", "ref2"]);
        let (board, warnings) = Storyboard::load(&source).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(board.item("note1").unwrap().row, "voice");
        assert_eq!(board.item("ref1").unwrap().row, "refs");
        match &board.item("ref2").unwrap().body {
            Body::Ref(reference) => assert_eq!((reference.role, reference.gen.seed, reference.src.as_ref().map(|s| s.asset_id.as_str())), (Role::Generate, Some(3), Some("ast_p"))),
            other => panic!("{other:?}"),
        }
        let read = run_ok("storyboard_read", "{}", &source);
        let items = read.json.get("items").unwrap().as_arr().unwrap();
        assert_eq!(items[0].get("id").and_then(Json::as_str), Some("hook"));
        assert_eq!(items.iter().find(|i| i.get("id").and_then(Json::as_str) == Some("scene1")).unwrap().get("scene"), Some(&Json::Int(2)));
        let moved = run_ok("storyboard_move", r#"{"moves": [{"id": "scene1", "start": 5, "length": 5}]}"#, &source);
        assert!(moved.json.get("changes").unwrap().as_str().unwrap().contains("retimed card `scene1` (scene 2, \"Demo\")"));
        source = moved.source.unwrap();
        let updated = run_ok("storyboard_update", r#"{"id": "hook", "mood": "", "look": "white type slams in"}"#, &source);
        source = updated.source.unwrap();
        assert!(source.contains("look: \"white type slams in\"") && !source.contains("mood:"), "{source}");
        let linked = run_ok("storyboard_link", r#"{"from": "hook", "to": "ref1", "label": "palette"}"#, &source);
        source = linked.source.unwrap();
        let set = run_ok("storyboard_set", r#"{"bpm": 96, "music": null, "style": "warm, grainy"}"#, &source);
        source = set.source.unwrap();
        let rows = run_ok("storyboard_row", r#"{"action": "add", "kind": "notes", "label": "Ideas"}"#, &source);
        source = rows.source.unwrap();
        let deleted = run_ok("storyboard_delete", r#"{"ids": ["hook"], "close_gap": true}"#, &source);
        source = deleted.source.unwrap();
        let (board, _) = Storyboard::load(&source).unwrap();
        assert_eq!((board.bpm, board.style.as_str(), board.links.len()), (Some(96.0), "warm, grainy", 0));
        assert_eq!(board.item("scene1").unwrap().start, 1.0);
        assert!(board.row("notes").is_some());
    }

    #[test]
    fn chapters_on_the_beat_and_a_shorts_frame() {
        let mut source = new_board("Showreel", 15.0, Some(120.0), "", "", "");
        source = run_ok("storyboard_row", r#"{"action": "add", "kind": "chapters"}"#, &source).source.unwrap();
        source = run_ok("storyboard_set", r#"{"layout": "shorts"}"#, &source).source.unwrap();
        let (board, _) = Storyboard::load(&source).unwrap();
        assert_eq!((board.layout.as_deref(), board.format), (Some("shorts"), Some((1080, 1920))));
        let added = run_ok("storyboard_add", r##"{"items": [
            {"kind": "chapter", "start": 2, "length": 1.75, "skill": "TYPOGRAPHY", "look": "I MAKE THINGS MOVE.", "hero": "the word MOVE.", "out": "zoom through the O", "color": "#ff5a1f", "band": "content"},
            {"kind": "chapter", "start": 0, "length": 2, "skill": "EASING", "hero": "the orange ball", "out": "the ball grows into the next ground", "color": "#0b0b0b", "band": "content"},
            {"kind": "note", "start": 0, "length": 3, "text": "ONE PROMPT.", "band": "headline"}
        ]}"##, &source);
        source = added.source.unwrap();
        let read = run_ok("storyboard_read", "{}", &source);
        let items = read.json.get("items").unwrap().as_arr().unwrap();
        let chapter = |id: &str| items.iter().find(|item| item.get("id").and_then(Json::as_str) == Some(id)).unwrap().clone();
        // Numbered in time order, whatever the order they were added in.
        assert_eq!(chapter("chapter2").get("chapter").and_then(Json::as_str), Some("01/02"));
        assert_eq!(chapter("chapter1").get("chapter").and_then(Json::as_str), Some("02/02"));
        assert_eq!(chapter("chapter1").get("on_beat"), Some(&Json::Bool(false)));
        assert_eq!(chapter("chapter2").get("on_beat"), Some(&Json::Bool(true)));
        assert_eq!(chapter("chapter1").get("band").and_then(Json::as_str), Some("content"));
        let layout = read.json.get("layout").unwrap();
        assert_eq!(layout.get("bands").and_then(Json::as_arr).map(|bands| bands.len()), Some(5));
        // A band the layout does not have is refused and named.
        let error = call("storyboard_update", &args(r#"{"id": "note1", "band": "sidebar"}"#), &source).unwrap().err().unwrap();
        assert!(error.contains("tag, headline, content, meta, floor"), "{error}");
        let (board, warnings) = Storyboard::load(&source).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(board.to_source(), Storyboard::load(&board.to_source()).unwrap().0.to_source());
        assert!(source.contains("layout: @shorts") && source.contains("band: @content"), "{source}");
    }

    #[test]
    fn treatments_pages_sections_and_shot_columns() {
        let mut source = new_board("Video", 30.0, Some(100.0), "", "", "");
        for kind in ["treatments", "bible", "sections"] {
            source = run_ok("storyboard_row", &format!(r#"{{"action": "add", "kind": "{kind}"}}"#), &source).source.unwrap();
        }
        source = run_ok("storyboard_add", r##"{"items": [
            {"kind": "treatment", "id": "t1", "start": 0, "length": 10, "title": "Paper city", "idea": "a city folds out of paper", "palette": ["#f0ebe0", "#ff5a1f"]},
            {"kind": "treatment", "id": "t2", "start": 10, "length": 10, "title": "Neon rain", "chosen": true},
            {"kind": "ref", "id": "sheet", "start": 0, "length": 5, "media_type": "motion", "label": "Character sheet"},
            {"kind": "page", "id": "p1", "start": 0, "length": 10, "title": "Palette and arc", "body": "Intro: cream. Drop: orange.", "refs": ["sheet"]},
            {"kind": "note", "row": "sections", "start": 8, "length": 8, "text": "drop 1: full and loud; the bass and kick come in"},
            {"kind": "scene", "id": "s1", "start": 0, "length": 4.8, "title": "Wake", "bars": "1–2", "camera": "slow push in", "why": "we meet her", "lyric": "morning"}
        ]}"##, &source).source.unwrap();
        // Choosing one un-chooses the other.
        source = run_ok("storyboard_update", r#"{"id": "t1", "chosen": true}"#, &source).source.unwrap();
        let (board, warnings) = Storyboard::load(&source).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let chosen: Vec<&str> = board.items.iter().filter(|item| matches!(&item.body, Body::Treatment(t) if t.chosen)).map(|item| item.id.as_str()).collect();
        assert_eq!(chosen, ["t1"]);
        match &board.item("s1").unwrap().body {
            Body::Scene(scene) => assert_eq!((scene.bars.as_str(), scene.camera.as_str(), scene.why.as_str(), scene.lyric.as_str()), ("1–2", "slow push in", "we meet her", "morning")),
            other => panic!("{other:?}"),
        }
        // Deleting a reference drops it from the pages that name it.
        source = run_ok("storyboard_delete", r#"{"ids": ["sheet"]}"#, &source).source.unwrap();
        let (board, warnings) = Storyboard::load(&source).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(matches!(&board.item("p1").unwrap().body, Body::Page(page) if page.refs.is_empty()));
        // Plans are not parts of the cut.
        let built = crate::build::infer(&board, &[], AssetRef { asset_id: "e".into(), revision: "r".into() });
        let board = Storyboard { built: Some(built), ..board };
        let (_, unbuilt) = crate::build::stale(&board);
        assert_eq!(unbuilt, ["s1"]);
    }

    #[test]
    fn lyric_lines_with_voices_and_devices() {
        let mut source = new_board("Film", 30.0, Some(120.0), "", "", "");
        for kind in ["lyrics", "devices"] {
            source = run_ok("storyboard_row", &format!(r#"{{"action": "add", "kind": "{kind}"}}"#), &source).source.unwrap();
        }
        // A voice before the legend exists is refused and says how to fix it.
        let error = call("storyboard_add", &args(r#"{"items": [{"kind": "lyric", "start": 0, "length": 2, "text": "x", "voice": "slam"}]}"#), &source).unwrap().err().unwrap();
        assert!(error.contains("lyric_film"), "{error}");
        source = run_ok("storyboard_set", r#"{"voices": "lyric_film"}"#, &source).source.unwrap();
        source = run_ok("storyboard_add", r#"{"items": [
            {"kind": "lyric", "id": "l1", "start": 2.5, "length": 3, "text": "A red thread of sun", "pun": "sunlight unspools", "set": "a spool on the quay", "voice": "slam", "device": "a thread of light unwinds"},
            {"kind": "device", "start": 0, "length": 5, "name": "Tide", "value": "0.04"},
            {"kind": "device", "start": 22, "length": 2, "name": "Tide", "value": "0.15"}
        ]}"#, &source).source.unwrap();
        let error = call("storyboard_update", &args(r#"{"id": "l1", "voice": "whisper"}"#), &source).unwrap().err().unwrap();
        assert!(error.contains("slam, wall, mono, serif, hand"), "{error}");
        let read = run_ok("storyboard_read", "{}", &source);
        assert_eq!(read.json.get("voices").and_then(Json::as_arr).map(|v| v.len()), Some(5));
        let devices = read.json.get("devices").and_then(Json::as_arr).unwrap();
        assert_eq!(devices[0].get("occurrences").and_then(Json::as_arr).map(|o| o.len()), Some(2));
        // A custom legend replaces the preset.
        source = run_ok("storyboard_set", r##"{"voices": [{"id": "shout", "label": "Shout", "face": "Anton", "color": "#ff0000"}]}"##, &source).source.unwrap();
        let (board, warnings) = Storyboard::load(&source).unwrap();
        assert_eq!(board.voices.len(), 1);
        // The line still names `slam`: a warning, not a broken board.
        assert!(warnings.iter().any(|w| w.message.contains("@slam")), "{warnings:?}");
        assert!(!source.contains("let slam"), "{source}");
    }

    #[test]
    fn bad_calls_are_explained() {
        let source = new_board("T", 10.0, None, "", "", "");
        let error = call("storyboard_add", &args(r#"{"items": [{"kind": "note", "start": 0, "length": 1, "title": "x"}]}"#), &source).unwrap().err().unwrap();
        assert!(error.contains("a note has no field `title`"), "{error}");
        let error = call("storyboard_update", &args(r#"{"id": "nope", "title": "x"}"#), &source).unwrap().err().unwrap();
        assert!(error.contains("there is no item `nope`"), "{error}");
        let error = call("storyboard_write", &args(r#"{"source": "let a = 1"}"#), &source).unwrap().err().unwrap();
        assert!(error.contains("ends with `Storyboard{"), "{error}");
        assert!(call("storyboard_apply_changes", &Json::Null, &source).is_none());
    }
}

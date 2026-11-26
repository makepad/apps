//! The effect catalogue of the content packs: the presets, a text check
//! of an effect document, and an effect published to the library.

use super::{done, fail, text, LibraryJob, ProductTools};
use makepad_app_shell::tool_host::{Ctx, Outcome};
use makepad_studio_api::ToolError;
use makepad_strict_json::{self as json, Value as Json};

pub fn list<P: ProductTools>(_: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let kind = text(args, "kind").unwrap_or("all");
    let query = text(args, "query").map(str::to_lowercase);
    let packs = makepad_content_packs::effect_packs();
    let effects: Vec<Json> = packs.iter().flat_map(|pack| pack.presets().iter().map(move |preset| (pack, preset))).filter_map(|(pack, (name, source))| {
        let transition = pack.is_transition(name);
        if (kind == "effect" && transition) || (kind == "transition" && !transition) {
            return None;
        }
        let title = pack.preset_title(name, source);
        if let Some(query) = &query {
            if !name.to_lowercase().contains(query) && !title.to_lowercase().contains(query) {
                return None;
            }
        }
        let refused = pack.seek_refusal(source, false);
        Some(json::obj(vec![
            ("id", json::s(*name)),
            ("title", json::s(title)),
            ("kind", json::s(if transition { "transition" } else { "effect" })),
            ("category", json::s(pack.preset_category(name).unwrap_or(""))),
            ("engine", json::s(pack.doc_facts(source).engine.unwrap_or_default())),
            ("cut_safe", Json::Bool(refused.is_none())),
            ("refused", refused.map(json::s).unwrap_or(Json::Null)),
        ]))
    }).collect();
    done(json::obj(vec![("effects", Json::Arr(effects))]))
}

pub fn read<P: ProductTools>(_: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let id = text(args, "id").unwrap_or("");
    match makepad_content_packs::effects::resolve(id) {
        Some((_, name, source)) => done(json::obj(vec![("id", json::s(name)), ("source", json::s(source))])),
        None => fail(ToolError::not_found(format!("no preset `{id}` (list_effects has the ids)"))),
    }
}

/// What a text check of an effect document says (the parse and the shader
/// compile happen where it is drawn), and why it would not be published.
fn facts(source: &str) -> (Json, Option<String>) {
    let pack = makepad_content_packs::effects::library();
    let facts = pack.as_ref().map(|pack| pack.doc_facts(source)).unwrap_or_default();
    let refused = match &pack {
        Some(pack) => pack.seek_refusal(source, false),
        None => Some(makepad_content_packs::effects::missing("an effect document")),
    };
    let category = makepad_asset_data::effect_doc::effect_category(source);
    let mut problems = Vec::new();
    if !source.contains('{') {
        problems.push("not a Splash document: an effect is `use mod.vjfx.*` … and a top-level object with `engine:`".to_string());
    }
    let value = json::obj(vec![
        ("engine", json::s(facts.engine.clone().unwrap_or_default())),
        ("category", json::s(category.name())),
        ("post", Json::Int(facts.post_stages as i64)),
        ("cut_safe", Json::Bool(refused.is_none())),
        ("refused", refused.clone().map(json::s).unwrap_or(Json::Null)),
        ("problems", Json::Arr(problems.iter().cloned().map(json::s).collect())),
        ("note", json::s("text-level check (engine, stages, seekability); shader compile errors come back when the effect is drawn")),
    ]);
    (value, refused.or_else(|| problems.first().cloned()))
}

pub fn validate<P: ProductTools>(_: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    done(facts(args.get("source").and_then(Json::as_str).unwrap_or("")).0)
}

pub fn write<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let source = args.get("source").and_then(Json::as_str).unwrap_or("").to_string();
    if let (_, Some(why)) = facts(&source) {
        return fail(ToolError::refused(format!("not published: {why}")));
    }
    let name = text(args, "name").unwrap_or("effect").to_string();
    let title = text(args, "title").map(str::to_string).unwrap_or_else(|| name.replace('_', " "));
    let transition = text(args, "kind") == Some("transition");
    P::library(ctx, LibraryJob::PublishEffect { name, title, transition, source })
}

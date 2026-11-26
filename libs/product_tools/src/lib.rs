//! The AI control API tools every product window has, written once over
//! the shell's tool host (`makepad_app_shell::tool_host`):
//!
//! * [`documents`] — the open document's tree and files: `tree_list`,
//!   `tree_search`, `doc_read`, `doc_write`, `doc_patch`, `doc_create`,
//!   `doc_rename`, `doc_delete`, `doc_history`, `doc_undo`, against the
//!   product's [`Documents`] (Story: its cut or deck; Stage: its score).
//! * [`library`] — the asset library: `search_library`, `asset_info`,
//!   `preview_asset`, `import_media`, and the jobs the other tools give the
//!   product's library worker.
//! * [`effects`] — the effect catalogue of the content packs:
//!   `list_effects`, `read_effect`, `validate_effect`, `write_effect`.
//! * [`gen`] — the generation tools over the product's generation studio
//!   (`gen_presets`, `gen_run`, `gen_job`, `gen_choose`, `gen_cancel`,
//!   `gen_estimate`, `gen_save_preset`).
//! * `stage_status` (the product's own fields, then the call log's and the
//!   registry's) and the compute-kernel library's `kernel_guide` /
//!   `kernel_check`.
//!
//! A product implements [`ProductTools`] on its tool marker and calls
//! [`register`] from its own registry; each product keeps its own registry,
//! waits and workers.

pub mod documents;
pub mod effects;
pub mod gen;
pub mod library;

use makepad_app_shell::tool_host::{Ctx, Documents, Handler, Outcome};
use makepad_asset_data::{AssetKind, AssetRevisionRef};
use makepad_studio_api::{self as api, Registry, ToolError, ToolOutput};
use makepad_strict_json::{self as json, Value as Json};

pub use library::{LibraryJob, LibraryOut};

/// What the shared tools need from a product besides its documents.
pub trait ProductTools: Documents {
    /// Run `job` on the product's library worker; the call is answered with
    /// the job's reply ([`library::run`] on the worker's thread).
    fn library(ctx: &mut Ctx<Self>, job: LibraryJob) -> Outcome<Self::Wait>;
    /// `stage_status`'s first fields: the product's page, document and
    /// work in flight.
    fn status(ctx: &mut Ctx<Self>) -> Vec<(&'static str, Json)>;
}

/// Register the shared tools on a product's registry.
pub fn register<P: ProductTools>(registry: &mut Registry<Handler<P>>) {
    let tools: &[(&str, Handler<P>)] = &[
        ("stage_status", stage_status::<P>),
        ("doc_undo", |ctx, args| P::undo(ctx, flag(args, "redo").unwrap_or(false))),
        ("tree_list", documents::tree_list::<P>),
        ("tree_search", documents::tree_search::<P>),
        ("doc_read", documents::read::<P>),
        ("doc_write", documents::write::<P>),
        ("doc_patch", documents::patch::<P>),
        ("doc_create", documents::create::<P>),
        ("doc_rename", documents::rename::<P>),
        ("doc_delete", documents::delete::<P>),
        ("doc_history", documents::history::<P>),
        ("list_effects", effects::list::<P>),
        ("read_effect", effects::read::<P>),
        ("validate_effect", effects::validate::<P>),
        ("write_effect", effects::write::<P>),
        ("search_library", library::search_tool::<P>),
        ("asset_info", library::asset_info_tool::<P>),
        ("import_media", library::import_media_tool::<P>),
        ("preview_asset", library::preview_tool::<P>),
        // The compute-kernel library's tools (makepad-kernel-tools), the
        // same in every app.
        ("kernel_guide", |ctx, args| kernel_tool(ctx, "kernel_guide", args)),
        ("kernel_check", |ctx, args| kernel_tool(ctx, "kernel_check", args)),
    ];
    for (name, handler) in tools {
        registry.register(name, *handler);
    }
}

fn stage_status<P: ProductTools>(ctx: &mut Ctx<P>, _: &Json) -> Outcome<P::Wait> {
    let mut fields = P::status(ctx);
    let available = ctx.host.registry.tools().iter().filter(|(_, available)| *available).count();
    fields.push(("last_event", Json::Int(ctx.host.log.last() as i64)));
    fields.push(("tools", json::obj(vec![("live", Json::Int(available as i64)), ("total", Json::Int(api::TOOLS.len() as i64))])));
    done(json::obj(fields))
}

/// kernel_guide / kernel_check: answered by the kernel library at once (a
/// check's dry run is admitted as untrusted work, at most half a second).
fn kernel_tool<P: ProductTools>(_: &mut Ctx<P>, name: &str, args: &Json) -> Outcome<P::Wait> {
    match makepad_render_kernels::tools::call(name, args, None) {
        Ok(value) => done(value),
        Err(message) => fail(ToolError::refused(message)),
    }
}

// ------------------------------------------------------------ helpers

/// A non-empty text argument, trimmed.
pub fn text<'a>(args: &'a Json, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Json::as_str).map(str::trim).filter(|text| !text.is_empty())
}

pub fn number(args: &Json, key: &str) -> Option<f64> {
    match args.get(key) {
        Some(Json::Int(value)) => Some(*value as f64),
        Some(Json::F64(value)) => Some(*value),
        _ => None,
    }
}

pub fn flag(args: &Json, key: &str) -> Option<bool> {
    args.get(key).and_then(Json::as_bool)
}

/// An `{asset_id, revision}` argument.
pub fn reference(value: Option<&Json>, what: &str) -> Result<AssetRevisionRef, ToolError> {
    let value = value.ok_or_else(|| ToolError::invalid(format!("`{what}` is missing")))?;
    let id = value.get("asset_id").and_then(Json::as_str).unwrap_or("");
    let revision = value.get("revision").and_then(Json::as_str).unwrap_or("");
    Ok(AssetRevisionRef {
        asset_id: id.parse().map_err(|_| ToolError::invalid(format!("`{what}.asset_id` `{id}` is not an asset id (32 hex digits, as search_library gives it)")))?,
        revision: revision.parse().map_err(|_| ToolError::invalid(format!("`{what}.revision` `{revision}` is not a revision id")))?,
    })
}

pub fn reference_json(reference: &AssetRevisionRef) -> Json {
    json::obj(vec![("asset_id", json::s(reference.asset_id.to_string())), ("revision", json::s(reference.revision.to_string()))])
}

/// A number as JSON: whole numbers as integers, others to six places.
pub fn num(value: f64) -> Json {
    if value.fract() == 0.0 && value.abs() < 1e15 { Json::Int(value as i64) } else { Json::F64((value * 1e6).round() / 1e6) }
}

pub fn done<W>(value: Json) -> Outcome<W> {
    Outcome::Done(Ok(ToolOutput::json(value)))
}

pub fn fail<W>(error: ToolError) -> Outcome<W> {
    Outcome::Done(Err(error))
}

/// An asset kind as the tools name it (`search_library`'s `kind`).
pub fn kind_name(kind: AssetKind) -> &'static str {
    match kind {
        AssetKind::Video => "video",
        AssetKind::Texture => "image",
        AssetKind::Audio => "audio",
        AssetKind::AppRun => "app_run",
        AssetKind::Cut => "cut",
        AssetKind::Deck => "deck",
        AssetKind::Tree => "tree",
        AssetKind::VjEffect => "effect",
        AssetKind::Flow => "flow",
        AssetKind::Motion => "motion",
        kind if kind.has_mesh() => "model3d",
        _ => "other",
    }
}

/// A tree file that is media, not text.
pub fn is_binary(kind: AssetKind) -> bool {
    matches!(kind, AssetKind::Texture | AssetKind::Video | AssetKind::Audio)
}

#[cfg(test)]
mod tests {
    /// The catalog lists the kernel library's tools byte for byte, and the
    /// guide the tools answer is the library's.
    #[test]
    fn shared_kernel_tools_are_the_librarys() {
        for def in makepad_render_kernels::tools::TOOLS {
            let spec = makepad_studio_api::find(def.api_name).unwrap_or_else(|| panic!("{} is not in the catalog", def.api_name));
            assert_eq!((spec.description, spec.input, spec.output), (def.description, def.input, def.output), "{}", def.api_name);
        }
        let out = makepad_render_kernels::tools::call("kernel_guide", &makepad_strict_json::Value::Obj(Vec::new()), None).unwrap();
        assert_eq!(out.get("guide").and_then(makepad_strict_json::Value::as_str), Some(makepad_render_kernels::tools::guide().as_str()));
    }
}

/// The characters of one `authoring_guide` page.
pub const GUIDE_PAGE: usize = 7_000;

/// The page of `text` from character `from`: it ends at a line end when one
/// is near, and `next` says where the following page starts.
pub fn guide_page(text: &str, from: usize) -> (&str, Option<usize>) {
    let mut start = from.min(text.len());
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let rest = &text[start..];
    if rest.len() <= GUIDE_PAGE {
        return (rest, None);
    }
    let mut end = GUIDE_PAGE;
    while !rest.is_char_boundary(end) {
        end -= 1;
    }
    if let Some(line_end) = rest[..end].rfind('\n').filter(|at| *at > GUIDE_PAGE / 2) {
        end = line_end + 1;
    }
    (&rest[..end], Some(start + end))
}

#[cfg(test)]
mod guide_tests {
    use super::{guide_page, GUIDE_PAGE};

    #[test]
    fn the_guide_comes_in_pages_that_join_into_the_whole() {
        let text: String = (0..3000).map(|n| format!("line {n} — with “quotes” and a tail\n")).collect();
        let (mut joined, mut from, mut pages) = (String::new(), 0, 0);
        loop {
            let (page, next) = guide_page(&text, from);
            assert!(page.len() <= GUIDE_PAGE && !page.is_empty());
            joined.push_str(page);
            pages += 1;
            match next {
                Some(next) => {
                    assert!(page.ends_with('\n'), "a page ends at a line end");
                    from = next;
                }
                None => break,
            }
        }
        assert_eq!(joined, text);
        assert!(pages > 5);
        // Past the end: an empty last page.
        assert_eq!(guide_page(&text, text.len() + 10), ("", None));
    }
}

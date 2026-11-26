//! The asset library tools and the library jobs the shared tools give the
//! product's library worker ([`ProductTools::library`]); [`run`] does a job
//! on that worker's thread, with the worker's own client.

use super::{fail, kind_name, num, reference, reference_json, text, number, ProductTools};
use makepad_app_shell::tool_host::{Ctx, Outcome};
use makepad_asset_client::api::CatalogQuery;
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionRef, FileRole, MediaType, ThumbnailMedia};
use makepad_studio_api::{self as api, ToolError};
use makepad_strict_json::{self as json, Value as Json};
use makepad_widgets::*;
use std::path::Path;

/// Library work: blocking, on the product's library worker.
pub enum LibraryJob {
    /// One page of a document file, with its outline and parse problems.
    ReadDocument { path: String, source: String, revision: u64, summary: Option<Json>, from_line: usize },
    /// The library revisions of a tree that changed `path` (or any file).
    TreeHistory { head: AssetRevisionRef, path: Option<String>, working: Json },
    Search { query: String, kind: String, tag: Option<String>, limit: u32 },
    AssetInfo { id: String, revision: Option<String> },
    Preview { asset: AssetRevisionRef },
    PublishEffect { name: String, title: String, transition: bool, source: String },
    ImportMedia { path: String, title: Option<String>, lyrics: Option<String>, music: bool, seconds: Option<f64> },
}

/// A library job's reply: the call's answer, with a picture.
pub enum LibraryOut {
    Json(Json),
    Png(Json, Vec<u8>),
}

/// Do `job`; `client` connects the worker's library client when the job
/// needs one.
pub fn run<'a>(job: LibraryJob, client: impl FnOnce() -> Result<&'a mut AssetClient, ToolError>) -> Result<LibraryOut, ToolError> {
    match job {
        LibraryJob::ReadDocument { path, source, revision, summary, from_line } => Ok(LibraryOut::Json(read_page(&path, &source, revision, summary, from_line))),
        LibraryJob::TreeHistory { head, path, working } => tree_history(client()?, head, path.as_deref(), working).map(LibraryOut::Json),
        LibraryJob::Search { query, kind, tag, limit } => search(client()?, &query, &kind, tag, limit).map(LibraryOut::Json),
        LibraryJob::AssetInfo { id, revision } => asset_info(client()?, &id, revision.as_deref()).map(LibraryOut::Json),
        LibraryJob::Preview { asset } => preview(client()?, &asset).map(|(info, png)| LibraryOut::Png(info, png)),
        LibraryJob::PublishEffect { name, title, transition, source } => publish_effect(client()?, &name, &title, transition, &source).map(LibraryOut::Json),
        LibraryJob::ImportMedia { path, title, lyrics, music, seconds } => import_media(client()?, &path, title.as_deref(), lyrics.as_deref(), music, seconds).map(LibraryOut::Json),
    }
}

/// One page of whole lines that fits the chat's result budget, and for a
/// file read in pages its outline (lets, named slides and the plates they
/// use, with lines), so the model reads only what it needs.
fn read_page(path: &str, source: &str, revision: u64, summary: Option<Json>, from_line: usize) -> Json {
    let (text, first, last, next) = api::document::page(source, from_line);
    let total = source.split_inclusive('\n').count().max(1);
    let paged = first > 1 || next.is_some();
    let mut diagnostics = Vec::new();
    let mut fields = vec![("path", json::s(path)), ("revision", Json::Int(revision as i64)), ("lines", Json::Arr(vec![Json::Int(first as i64), Json::Int(last as i64)])), ("total_lines", Json::Int(total as i64))];
    if let Some(next) = next {
        fields.push(("next_line", Json::Int(next as i64)));
    }
    if paged && first == 1 && path.ends_with(".splash") {
        match api::document::outline(source) {
            Ok(outline) => fields.push(("outline", outline)),
            Err(error) => diagnostics.push(json::s(format!("{path}: {error}"))),
        }
    }
    if !paged && path.ends_with(".splash") {
        if let Err(error) = makepad_script::syntax::parse(source) {
            diagnostics.push(json::s(format!("{path}: {error}")));
        }
    }
    fields.push(("text", json::s(text)));
    fields.push(("diagnostics", Json::Arr(diagnostics)));
    if let Some(summary) = summary {
        fields.push(("document", summary));
    }
    json::obj(fields)
}

// ------------------------------------------------------------ tools

pub fn search_tool<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let kind = text(args, "kind").unwrap_or("all").to_string();
    let limit = args.get("limit").and_then(Json::as_u64).unwrap_or(20) as u32;
    P::library(ctx, LibraryJob::Search { query: text(args, "query").unwrap_or("").to_string(), kind, tag: text(args, "tag").map(str::to_string), limit })
}

pub fn asset_info_tool<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    P::library(ctx, LibraryJob::AssetInfo { id: text(args, "asset_id").unwrap_or("").to_string(), revision: text(args, "revision").map(str::to_string) })
}

pub fn preview_tool<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    match reference(args.get("asset"), "asset") {
        Ok(asset) => P::library(ctx, LibraryJob::Preview { asset }),
        Err(error) => fail(error),
    }
}

pub fn import_media_tool<P: ProductTools>(ctx: &mut Ctx<P>, args: &Json) -> Outcome<P::Wait> {
    let path = text(args, "path").unwrap_or("").to_string();
    let lyrics = text(args, "lyrics").map(str::to_string);
    let music = args.get("music").and_then(Json::as_bool).unwrap_or(lyrics.is_some());
    let seconds = number(args, "seconds");
    P::library(ctx, LibraryJob::ImportMedia { path, title: text(args, "title").map(str::to_string), lyrics, music, seconds })
}

// ------------------------------------------------------------ the library

/// A tree asset's published revisions, oldest first.
pub fn tree_revisions(client: &mut AssetClient, head: AssetRevisionRef) -> Result<Vec<AssetRevisionRef>, ToolError> {
    let mut candidates = client.asset_detail(&head.asset_id).map_err(|error| ToolError::failed(error.to_string()))?.candidates;
    candidates.retain(|candidate| candidate.state == makepad_asset_client::dto::CandidateStateDto::Published);
    candidates.sort_by_key(|candidate| (candidate.published_ms, candidate.revision));
    Ok(candidates.into_iter().map(|candidate| AssetRevisionRef { asset_id: head.asset_id, revision: candidate.revision }).collect())
}

fn tree_history(client: &mut AssetClient, head: AssetRevisionRef, path: Option<&str>, working: Json) -> Result<Json, ToolError> {
    use makepad_asset_client::tree::{Tree, TreeStore};
    let mut previous = Tree::default();
    let mut history = Vec::new();
    for (index, reference) in tree_revisions(client, head)?.into_iter().enumerate() {
        let content = TreeStore::read(client, reference).map_err(ToolError::failed)?;
        let tree = Tree::parse(std::str::from_utf8(&content.bytes).map_err(|_| ToolError::failed("tree manifest is not text"))?).map_err(ToolError::failed)?;
        let mut paths: Vec<_> = previous.entries.keys().chain(tree.entries.keys()).filter(|file| path.is_none_or(|path| file.as_str() == path)).cloned().collect();
        paths.sort();
        paths.dedup();
        paths.retain(|path| previous.entries.get(path) != tree.entries.get(path));
        if path.is_none() || !paths.is_empty() {
            history.push(json::obj(vec![("revision", Json::Int(index as i64 + 1)), ("paths", Json::Arr(paths.into_iter().map(json::s).collect()))]));
        }
        previous = tree;
    }
    Ok(json::obj(vec![("history", Json::Arr(history)), ("working", working), ("unit", json::s("tree revision"))]))
}

fn search(client: &mut AssetClient, text: &str, kind: &str, tag: Option<String>, limit: u32) -> Result<Json, ToolError> {
    let limit = limit.clamp(1, 100);
    let wanted = match kind {
        "video" => Some(AssetKind::Video),
        "image" => Some(AssetKind::Texture),
        "audio" => Some(AssetKind::Audio),
        "app_run" => Some(AssetKind::AppRun),
        "cut" => Some(AssetKind::Cut),
        "deck" => Some(AssetKind::Deck),
        "tree" => Some(AssetKind::Tree),
        "effect" => Some(AssetKind::VjEffect),
        "flow" => Some(AssetKind::Flow),
        "motion" => Some(AssetKind::Motion),
        _ => None,
    };
    let models = kind == "model3d";
    // 3D models are several kinds: search wide and keep the mesh-bearing.
    let page_size = if models { 100 } else { limit };
    let mut query = if text.trim().is_empty() { CatalogQuery::browse(page_size) } else { CatalogQuery::text(text.trim(), page_size) };
    query.kind = wanted;
    query.newest = true;
    query.tag = tag;
    let page = client.catalog_search(&query, None).map_err(|error| ToolError::failed(error.to_string()))?;
    let mut items = Vec::new();
    for hit in page.hits {
        if items.len() >= limit as usize {
            break;
        }
        if models && !hit.kind.is_some_and(AssetKind::has_mesh) {
            continue;
        }
        let Ok(detail) = client.asset_detail(&hit.asset_id) else { continue };
        if detail.retired {
            continue;
        }
        let Some(head) = detail.latest_published() else { continue };
        let reference = AssetRevisionRef { asset_id: detail.asset_id, revision: head.revision };
        let manifest = client.resolve_ref(&reference).ok();
        let mut fields = vec![
            ("asset_id", json::s(reference.asset_id.to_string())),
            ("revision", json::s(reference.revision.to_string())),
            ("kind", json::s(hit.kind.map(kind_name).unwrap_or("other"))),
            ("title", json::s(if hit.title.is_empty() { reference.asset_id.to_string() } else { hit.title.clone() })),
        ];
        if let Some(manifest) = &manifest {
            if manifest.metrics.media_millis > 0 {
                fields.push(("seconds", num(manifest.metrics.media_millis as f64 / 1000.0)));
            }
            if let Some(dims) = manifest.files.iter().find_map(|file| file.dims) {
                fields.push(("width", Json::Int(dims.width as i64)));
                fields.push(("height", Json::Int(dims.height as i64)));
            }
        }
        if matches!(hit.kind, Some(AssetKind::AppRun) | Some(AssetKind::Cut)) {
            if let Ok(Some((render, from))) = client.render_of(&reference.asset_id) {
                fields.push(("render", reference_json(&render)));
                fields.push(("render_fresh", Json::Bool(from.revision == reference.revision)));
            }
        }
        if !hit.snippet.is_empty() {
            fields.push(("snippet", json::s(hit.snippet.clone())));
        }
        items.push(json::obj(fields));
    }
    Ok(json::obj(vec![("items", Json::Arr(items))]))
}

fn asset_info(client: &mut AssetClient, id: &str, revision: Option<&str>) -> Result<Json, ToolError> {
    let asset_id: AssetId = id.parse().map_err(|_| ToolError::invalid(format!("`{id}` is not an asset id")))?;
    let detail = client.asset_detail(&asset_id).map_err(|error| ToolError::not_found(error.to_string()))?;
    let head = detail.latest_published().map(|head| head.revision);
    let revision = match revision {
        Some(revision) => revision.parse().map_err(|_| ToolError::invalid(format!("`{revision}` is not a revision")))?,
        None => head.ok_or_else(|| ToolError::not_found("the asset has no published revision"))?,
    };
    let reference = AssetRevisionRef { asset_id, revision };
    let manifest = client.resolve_ref(&reference).map_err(|error| ToolError::failed(error.to_string()))?;
    let note = client.get_annotation(&asset_id).ok();
    let files: Vec<Json> = manifest.files.iter().map(|file| {
        let mut fields = vec![("role", json::s(format!("{:?}", file.role))), ("media", json::s(format!("{:?}", file.media))), ("bytes", Json::Int(file.byte_len as i64))];
        if let Some(dims) = file.dims {
            fields.push(("width", Json::Int(dims.width as i64)));
            fields.push(("height", Json::Int(dims.height as i64)));
        }
        json::obj(fields)
    }).collect();
    let mut fields = vec![
        ("asset", reference_json(&reference)),
        ("kind", json::s(kind_name(manifest.kind))),
        ("title", json::s(note.as_ref().map(|note| note.title.clone()).unwrap_or_default())),
        ("head", head.map(|head| json::s(head.to_string())).unwrap_or(Json::Null)),
        ("retired", Json::Bool(detail.retired)),
        ("files", Json::Arr(files)),
        ("tags", Json::Arr(note.map(|note| note.tags.into_iter().map(json::s).collect()).unwrap_or_default())),
        ("dependencies", Json::Arr(manifest.dependencies.iter().map(reference_json).collect())),
    ];
    if manifest.metrics.media_millis > 0 {
        fields.push(("seconds", num(manifest.metrics.media_millis as f64 / 1000.0)));
    }
    if let Ok(Some((render, from))) = client.render_of(&asset_id) {
        fields.push(("render", reference_json(&render)));
        fields.push(("render_fresh", Json::Bool(from.revision == reference.revision)));
    }
    if let Ok(users) = client.dependents(&asset_id, None) {
        fields.push(("used_by", Json::Int(users.total as i64)));
    }
    Ok(json::obj(fields))
}

fn preview(client: &mut AssetClient, asset: &AssetRevisionRef) -> Result<(Json, Vec<u8>), ToolError> {
    let manifest = client.resolve_ref(asset).map_err(|error| ToolError::failed(error.to_string()))?;
    let meta = manifest.thumbnail.as_ref().ok_or_else(|| ToolError::not_found("the asset has no thumbnail"))?;
    let bytes = client.fetch_blob_bytes(&meta.blob, Some(meta.byte_len)).map_err(|error| ToolError::failed(error.to_string()))?;
    // Pictures go out as PNG: decode the thumbnail and re-encode it.
    let image = decode_image_from_data(&bytes).map_err(|error| ToolError::failed(format!("the thumbnail does not decode: {error:?}")))?;
    let png = png_of(&image.data, image.width, image.height)?;
    Ok((json::obj(vec![("width", Json::Int(image.width as i64)), ("height", Json::Int(image.height as i64)), ("title", json::s(format!("thumbnail of {}", asset.asset_id)))]), png))
}

/// 0xAARRGGBB pixels as a PNG.
pub fn png_of(pixels: &[u32], width: usize, height: usize) -> Result<Vec<u8>, ToolError> {
    let rgba: Vec<u8> = pixels.iter().flat_map(|pixel| [(pixel >> 16) as u8, (pixel >> 8) as u8, *pixel as u8, (pixel >> 24) as u8]).collect();
    Cx::encode_rgba_as_png(width as u32, height as u32, &rgba).map_err(|error| ToolError::failed(format!("png: {error}")))
}

/// A published asset's thumbnail when it has no picture of its own: the
/// effect library's placeholder for `name`.
pub fn placeholder_jpeg(name: &str) -> Result<PublishThumbnail, ToolError> {
    let (w, h) = (448usize, 256usize);
    let bgra: Vec<u8> = makepad_content_packs::effects::placeholder_bgra(name, w, h).iter().flat_map(|pixel| pixel.to_le_bytes()).collect();
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 85).encode(&bgra, w as u16, h as u16, jpeg_encoder::ColorType::Bgra).map_err(|error| ToolError::failed(format!("thumbnail: {error}")))?;
    Ok(PublishThumbnail::plain(jpeg, ThumbnailMedia::Jpeg, w as u32, h as u32))
}

fn publish_effect(client: &mut AssetClient, name: &str, title: &str, transition: bool, source: &str) -> Result<Json, ToolError> {
    let thumbnail = placeholder_jpeg(name)?;
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)];
    let mut bundle = PublishBundle::new("stage", AssetKind::VjEffect, title.to_string(), files, thumbnail, PublishRights::generated_cc0());
    bundle.generator = "stage-studio-api".into();
    let mut tags = vec!["cut".to_string(), if transition { "transition".to_string() } else { makepad_asset_data::effect_doc::effect_category(source).name().to_string() }];
    tags.retain(|tag| !tag.is_empty());
    bundle.tags = tags;
    let published = client.publish_bundle(&bundle).map_err(|error| ToolError::failed(error.to_string()))?;
    let reference = AssetRevisionRef { asset_id: published.asset_id, revision: published.revision };
    Ok(json::obj(vec![
        ("effect", json::s(format!("asset:{}:{}", reference.asset_id, reference.revision))),
        ("asset", reference_json(&reference)),
        ("name", json::s(name)),
    ]))
}

/// A song file as mono samples (for its length and thumbnail), its rate
/// and its media type: WAV or MP3.
pub fn decode_song(path: &Path, bytes: &[u8]) -> Result<(MediaType, Vec<f32>, f64), String> {
    let ext = path.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "wav" => {
            let pcm = makepad_stage_audio_media::decode::parse_wav(bytes, usize::MAX)?;
            let mono = pcm.frames.iter().map(|[l, r]| (*l as f32 + *r as f32) * (0.5 / 32768.0)).collect();
            Ok((MediaType::Wav, mono, pcm.sample_rate as f64))
        }
        "mp3" => {
            let audio = makepad_audio_decode::decode_any(bytes).map_err(|error| format!("{}: {error:?}", path.display()))?;
            let left = audio.channel(0);
            let mono = if audio.channels > 1 { left.iter().zip(&audio.channel(1)).map(|(l, r)| (l + r) * 0.5).collect() } else { left };
            Ok((MediaType::Mp3, mono, audio.rate as f64))
        }
        _ => Err(format!("{}: give the song as a WAV or MP3 file", path.display())),
    }
}

/// The largest file import_media reads (a feature film's MP4 fits).
const IMPORT_MAX_BYTES: u64 = 4 << 30;

/// Reads a local media file (absolute path, a regular file) and publishes it
/// as a library asset of its kind: audio measured by decoding, lyrics as the
/// asset's Lyrics file, user-supplied rights kept on this LAN.
fn import_media(client: &mut AssetClient, path: &str, title: Option<&str>, lyrics: Option<&str>, music: bool, seconds: Option<f64>) -> Result<Json, ToolError> {
    let read = |path: &str, what: &str| -> Result<Vec<u8>, ToolError> {
        let p = Path::new(path);
        if !p.is_absolute() {
            return Err(ToolError::invalid(format!("{what}: give an absolute path, not `{path}`")));
        }
        let meta = std::fs::metadata(p).map_err(|error| ToolError::not_found(format!("{what}: {path}: {error}")))?;
        if !meta.is_file() {
            return Err(ToolError::invalid(format!("{what}: {path} is not a file")));
        }
        if meta.len() > IMPORT_MAX_BYTES {
            return Err(ToolError::refused(format!("{what}: {path} is {} bytes, over {IMPORT_MAX_BYTES}", meta.len())));
        }
        std::fs::read(p).map_err(|error| ToolError::failed(format!("{what}: {path}: {error}")))
    };
    let bytes = read(path, "path")?;
    let p = Path::new(path);
    let ext = p.extension().and_then(|ext| ext.to_str()).unwrap_or("").to_ascii_lowercase();
    let name = title.map(str::to_string).unwrap_or_else(|| p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "Imported".into()));
    let rights = PublishRights::declared("LicenseRef-user-supplied", "", path.to_string(), makepad_asset_data::Redistribution::LanLocal, makepad_asset_data::DerivativePolicy::LocalPreview);
    let mut bundle = match ext.as_str() {
        "mp3" | "wav" => {
            let (media, mono, rate) = decode_song(p, &bytes).map_err(ToolError::invalid)?;
            let secs = mono.len() as f64 / rate.max(1.0);
            let thumbnail = makepad_story_edit_voice::publish::waveform_thumbnail(&mono).map_err(ToolError::failed)?;
            let mut files = vec![PublishBundleFile::bytes(FileRole::Audio, media, bytes, None)];
            if let Some(lyrics) = lyrics {
                let json = read(lyrics, "lyrics")?;
                if makepad_strict_json::parse_depth(&json, 16).is_err() {
                    return Err(ToolError::invalid(format!("lyrics: {lyrics} is not JSON")));
                }
                files.push(PublishBundleFile::bytes(FileRole::Lyrics, MediaType::Json, json, None));
            }
            let mut b = PublishBundle::new("stage", AssetKind::Audio, name.clone(), files, thumbnail, rights);
            b.media_millis = ((secs * 1000.0).round() as u32).max(1);
            if music {
                b.categories = vec!["music".into()];
                b.tags = vec!["music".into(), "song".into()];
            }
            b
        }
        "png" | "jpg" | "jpeg" => {
            let media = if ext == "png" { MediaType::Png } else { MediaType::Jpeg };
            let dims = if ext == "png" { makepad_asset_data::image_dims::png_dims(&bytes) } else { makepad_asset_data::image_dims::jpeg_dims(&bytes) }
                .ok_or_else(|| ToolError::invalid(format!("path: not a readable {ext} image")))?;
            PublishBundle::new("stage", AssetKind::Texture, name.clone(), vec![PublishBundleFile::bytes(FileRole::Texture, media, bytes, Some(dims))], placeholder_jpeg(&name)?, rights)
        }
        "mp4" => {
            let mut b = PublishBundle::new("stage", AssetKind::Video, name.clone(), vec![PublishBundleFile::bytes(FileRole::Video, MediaType::Mp4, bytes, None)], placeholder_jpeg(&name)?, rights);
            b.media_millis = ((seconds.unwrap_or(0.0) * 1000.0).round() as u32).max(1);
            b
        }
        _ => return Err(ToolError::invalid(format!("path: `.{ext}` is not a media file import_media takes (mp3, wav, png, jpg, mp4)"))),
    };
    bundle.generator = "stage-studio-api".into();
    let published = client.publish_bundle(&bundle).map_err(|error| ToolError::failed(error.to_string()))?;
    let reference = AssetRevisionRef { asset_id: published.asset_id, revision: published.revision };
    Ok(json::obj(vec![
        ("asset", reference_json(&reference)),
        ("title", json::s(name)),
        ("use", json::s(format!("asset(\"{}\", \"{}\")", reference.asset_id, reference.revision))),
    ]))
}

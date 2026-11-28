//! The asset library as a `ResultStore`: today's asset worker (a long-lived
//! thread that owns the asset client) behind the open-source flow engine's
//! result seam. Stage, Story, Sandbox and the commercial flow-server install
//! an [`AssetWorkerHandle`] where the engine takes `Arc<dyn ResultStore>`.

use makepad_flow::engine::results::{
    PublishEvent, ResultBytes, ResultFiles, ResultInfo, ResultKind, ResultMedia, ResultPage, ResultQuery,
    ResultRef, ResultRole, ResultStore, PRESET_TAG,
};
use makepad_flow::engine::thumbnail::{image_thumbnail, placeholder_thumbnail, video_thumbnail};
use makepad_flow::{AssetsResponse, FlowAsset};
use makepad_asset_client::{
    content_client_caps, ApiEndpoints, AssetClient, CatalogHit, CatalogQuery, ClientConfig,
    ClientError, DiscoveryListener, PublishFile, PublishProvenance, PublishRequest, PublishThumbnail,
    Published,
};
use makepad_asset_data::{
    AssetAlias, AssetId, AssetKind, AssetRevisionId, FileRole, MediaType, ThumbnailMedia,
};
use makepad_strict_json::Value as Json;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const DISCOVERY_TTL_MS: u64 = 10_000;

#[derive(Clone, Debug)]
pub struct AssetStoreConfig {
    /// Archive generated media and terminal outputs in hosts with a configured asset store.
    pub archive_outputs: bool,
    pub cache_dir: PathBuf,
    pub token: Option<String>,
    pub endpoints: Option<ApiEndpoints>,
    pub server_id: Option<[u8; 16]>,
    pub discovery_port: u16,
    pub discovery_wait_ms: u64,
    /// Where the host's store is NOW, when it can move (an app that takes
    /// its store over from a host that quit): asked before each command,
    /// and a client made for other endpoints is replaced. `None` keeps the
    /// fixed `endpoints`, `server_id` and `token` above.
    pub locate: Option<StoreLocator>,
    /// The namespace [`ResultStore::find_by_digest`] looks a fixed-seed
    /// result up in (its reuse alias lives there).
    pub namespace: String,
}

/// The host's answer to "where is the store now": endpoints, server id,
/// credential. See [`AssetStoreConfig::locate`].
#[derive(Clone)]
pub struct StoreLocator(pub std::sync::Arc<dyn Fn() -> Result<(ApiEndpoints, [u8; 16], String), String> + Send + Sync>);

impl std::fmt::Debug for StoreLocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StoreLocator")
    }
}

impl AssetStoreConfig {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self {
            archive_outputs: false,
            cache_dir,
            token: None,
            endpoints: None,
            server_id: None,
            discovery_port: makepad_asset_client::wire::DEFAULT_DISCOVERY_PORT,
            discovery_wait_ms: 1_000,
            locate: None,
            namespace: "flows".to_string(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssetListQuery {
    pub text: String,
    /// `None` widens the query to every namespace. `Some("flows")` also
    /// includes assets carrying the conventional `flow` tag.
    pub namespace: Option<String>,
    pub limit: u32,
    pub cursor: Option<String>,
}

pub enum PublishWorkerEvent {
    Progress(&'static str),
    Done(Result<Published, String>),
}

/// Where a published value came from, resolved on the asset worker: the
/// digests of the media upstream of it (library inputs among them become
/// the revision's parents) and the provenance record to complete.
#[derive(Clone, Debug, Default)]
pub struct PublishLineage {
    /// The published value's own digest, never its own parent.
    pub value: [u8; 32],
    pub sources: Vec<[u8; 32]>,
    /// The provenance record's fields; the worker adds `parents`.
    pub provenance: Vec<(String, Json)>,
}

#[derive(Clone, Copy)]
enum ReadMode {
    Content,
    Preview,
    Thumbnail,
}

enum AssetCommand {
    Publish(PublishRequest, PublishLineage, Sender<PublishWorkerEvent>),
    NoteOrigin([u8; 32], AssetId, AssetRevisionId),
    List(AssetListQuery, Sender<Result<AssetsResponse, String>>),
    Thumbnail(AssetAlias, Sender<Result<ResultBytes, String>>),
    Read(AssetId, bool, Sender<Result<ResultBytes, String>>),
    PublishFiles(ResultFiles, Sender<PublishEvent>),
    FindDigest(String, Sender<Option<ResultRef>>),
    ReadRef(ResultRef, ReadMode, Sender<Result<ResultBytes, String>>),
    Info(ResultRef, Sender<Option<ResultInfo>>),
    NoteOriginRef([u8; 32], ResultRef),
    ReadPreview(AssetId, Sender<Result<ResultBytes, String>>),
    Stop,
}

#[derive(Clone)]
pub struct AssetWorkerHandle {
    tx: Sender<AssetCommand>,
    archive_outputs: bool,
}

impl AssetWorkerHandle {
    pub fn read_asset(&self, id: AssetId, thumbnail: bool) -> Result<ResultBytes, String> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(AssetCommand::Read(id, thumbnail, tx))
            .map_err(|_| "asset worker is not running".to_string())?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| "asset content request timed out".to_string())?
    }

    /// Read the small, safe representation used by the asset library cards.
    /// Full content remains available through `read_asset(id, false)` for the
    /// viewer; callers must opt into this bound explicitly.
    pub fn read_asset_preview(&self, id: AssetId) -> Result<ResultBytes, String> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(AssetCommand::ReadPreview(id, tx))
            .map_err(|_| "asset worker is not running".to_string())?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| "asset preview request timed out".to_string())?
    }

    pub fn publish(&self, request: PublishRequest) -> Result<Receiver<PublishWorkerEvent>, String> {
        self.publish_with_lineage(request, PublishLineage::default())
    }

    /// Publish with the run's lineage: upstream values this worker knows
    /// came from the library become the revision's parents.
    pub fn publish_with_lineage(
        &self,
        request: PublishRequest,
        lineage: PublishLineage,
    ) -> Result<Receiver<PublishWorkerEvent>, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(AssetCommand::Publish(request, lineage, tx))
            .map_err(|_| "asset worker is not running".to_string())?;
        Ok(rx)
    }

    /// Tell the worker that the value with this digest (the SHA-256 of its
    /// bytes, `Value::digest`) is the file of a library revision, so a flow
    /// that publishes something made from it records that revision as a
    /// parent. Content read through [`Self::read_asset`], and everything
    /// this worker publishes, is noted on its own; a host that feeds a run
    /// library media it read through another client notes it here. Never
    /// blocks.
    pub fn note_origin(&self, digest: [u8; 32], asset: AssetId, revision: AssetRevisionId) {
        let _ = self.tx.send(AssetCommand::NoteOrigin(digest, asset, revision));
    }

    pub fn list(&self, query: AssetListQuery) -> Result<Vec<FlowAsset>, String> {
        self.list_page(query).map(|page| page.assets)
    }

    pub fn list_page(&self, query: AssetListQuery) -> Result<AssetsResponse, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(AssetCommand::List(query, tx))
            .map_err(|_| "asset worker is not running".to_string())?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|_| "asset library request timed out".to_string())?
    }

    pub fn thumbnail(&self, alias: AssetAlias) -> Result<ResultBytes, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(AssetCommand::Thumbnail(alias, tx))
            .map_err(|_| "asset worker is not running".to_string())?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|_| "asset thumbnail request timed out".to_string())?
    }
}

pub struct AssetWorker {
    handle: AssetWorkerHandle,
    join: Option<JoinHandle<()>>,
}

impl AssetWorker {
    pub fn start(config: AssetStoreConfig) -> Result<Self, String> {
        let (handle, join) = spawn_worker(config)?;
        Ok(Self { handle, join: Some(join) })
    }

    /// A worker nobody joins: it ends when the last handle (the engine's
    /// store, for a host that hands it over) is dropped.
    pub fn start_detached(config: AssetStoreConfig) -> Result<AssetWorkerHandle, String> {
        spawn_worker(config).map(|(handle, _)| handle)
    }

    pub fn handle(&self) -> AssetWorkerHandle {
        self.handle.clone()
    }

    pub fn stop(&mut self) {
        let _ = self.handle.tx.send(AssetCommand::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for AssetWorker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn spawn_worker(config: AssetStoreConfig) -> Result<(AssetWorkerHandle, JoinHandle<()>), String> {
    let (tx, rx) = mpsc::channel();
    let handle = AssetWorkerHandle { tx, archive_outputs: config.archive_outputs };
    let join = std::thread::Builder::new()
        .name("flow-asset-store".to_string())
        .spawn(move || worker_loop(config, rx))
        .map_err(|error| format!("start asset worker: {error}"))?;
    Ok((handle, join))
}

fn worker_loop(config: AssetStoreConfig, rx: Receiver<AssetCommand>) {
    let mut client = None;
    let mut pages: HashMap<String, (Instant, AssetPageCursor)> = HashMap::new();
    // Library revisions by the digest of their file: what lets a publish
    // name the library media its value was made from.
    let mut origins: HashMap<[u8; 32], (AssetId, AssetRevisionId)> = HashMap::new();
    while let Ok(command) = rx.recv() {
        match command {
            AssetCommand::Stop => break,
            AssetCommand::NoteOrigin(digest, asset, revision) => {
                origins.insert(digest, (asset, revision));
            }
            AssetCommand::Read(id, thumbnail, reply) => {
                let result = connected_client(&config, &mut client)
                    .and_then(|client| read_asset(client, id, None, thumbnail, &mut origins));
                let _ = reply.send(result);
            }
            AssetCommand::ReadPreview(id, reply) => {
                let result = connected_client(&config, &mut client)
                    .and_then(|client| read_asset_preview(client, id, None));
                let _ = reply.send(result);
            }
            AssetCommand::Publish(mut request, lineage, reply) => {
                let result = publish_request(&config, &mut client, &mut origins, &mut request, &lineage, |stage| {
                    let _ = reply.send(PublishWorkerEvent::Progress(stage));
                });
                let _ = reply.send(PublishWorkerEvent::Done(result));
            }
            AssetCommand::PublishFiles(files, reply) => {
                let lineage = PublishLineage {
                    value: files.value_digest,
                    sources: files.sources.clone(),
                    provenance: files.provenance.clone(),
                };
                let result = to_request(files).and_then(|mut request| {
                    publish_request(&config, &mut client, &mut origins, &mut request, &lineage, |stage| {
                        let _ = reply.send(PublishEvent::Progress(stage));
                    })
                });
                let _ = reply.send(PublishEvent::Done(result.map(|published| ResultRef {
                    id: published.asset_id.to_string(),
                    revision: Some(published.revision.to_string()),
                    alias: published.alias.as_ref().map(ToString::to_string),
                })));
            }
            AssetCommand::FindDigest(digest, reply) => {
                let found = connected_client(&config, &mut client)
                    .ok()
                    .and_then(|client| find_by_digest(client, &config.namespace, &digest));
                let _ = reply.send(found);
            }
            AssetCommand::ReadRef(result, mode, reply) => {
                let answer = connected_client(&config, &mut client).and_then(|client| match (mode, &result) {
                    (ReadMode::Thumbnail, ResultRef { id, alias: Some(alias), .. }) if id.is_empty() => {
                        let alias = AssetAlias::new(alias.clone()).map_err(|_| "invalid asset alias".to_string())?;
                        thumbnail_of_alias(client, &alias)
                    }
                    _ => {
                        let id = resolve_id(client, &result)?;
                        let revision = ref_revision(&result)?;
                        match mode {
                            ReadMode::Content => read_asset(client, id, revision, false, &mut origins),
                            ReadMode::Thumbnail => read_asset(client, id, revision, true, &mut origins),
                            ReadMode::Preview => read_asset_preview(client, id, revision),
                        }
                    }
                });
                let _ = reply.send(answer);
            }
            AssetCommand::Info(result, reply) => {
                let info = connected_client(&config, &mut client).ok().and_then(|client| info_of(client, &result));
                let _ = reply.send(info);
            }
            AssetCommand::NoteOriginRef(digest, result) => {
                if let Ok(client) = connected_client(&config, &mut client) {
                    if let Ok(id) = resolve_id(client, &result) {
                        let revision = match result.revision.as_deref().map(str::parse::<AssetRevisionId>) {
                            Some(Ok(revision)) => Some(revision),
                            _ => client.asset_detail(&id).ok().and_then(|detail| detail.latest_published().map(|c| c.revision)),
                        };
                        if let Some(revision) = revision {
                            origins.insert(digest, (id, revision));
                        }
                    }
                }
            }
            AssetCommand::List(query, reply) => {
                let result = connected_client(&config, &mut client)
                    .and_then(|client| list_assets(client, &query, &mut pages));
                let _ = reply.send(result);
            }
            AssetCommand::Thumbnail(alias, reply) => {
                let result = connected_client(&config, &mut client).and_then(|client| thumbnail_of_alias(client, &alias));
                let _ = reply.send(result);
            }
        }
    }
}

fn thumbnail_of_alias(client: &mut AssetClient, alias: &AssetAlias) -> Result<ResultBytes, String> {
    let bytes = client
        .thumbnail_alias_bytes(alias)
        .map_err(|error| format!("fetch asset thumbnail: {error}"))?;
    let content_type = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        "image/jpeg"
    } else {
        return Err("asset thumbnail has an unsupported image type".to_string());
    };
    Ok(ResultBytes { content_type: content_type.to_string(), bytes })
}

/// One publication on the worker thread: lineage, the video poster, alias
/// reuse, the upload. `progress` names the stages as they pass.
fn publish_request(
    config: &AssetStoreConfig,
    client: &mut Option<AssetClient>,
    origins: &mut HashMap<[u8; 32], (AssetId, AssetRevisionId)>,
    request: &mut PublishRequest,
    lineage: &PublishLineage,
    progress: impl Fn(&'static str),
) -> Result<Published, String> {
    let parents = attach_lineage(request, lineage, origins);
    if !parents.is_empty() {
        eprintln!("[flow] publish `{}`: made from {} library revision(s)", request.title, parents.len());
    }
    let result = (|| {
        let client = connected_client(config, client)?;
        if request.artifact.media == MediaType::Mp4 {
            // Decode on this long-lived asset worker, never on the
            // engine/UI thread. Keep the original MP4 intact.
            match video_thumbnail(&request.artifact.bytes) {
                Ok(thumbnail) => request.thumbnail = plain_thumbnail(thumbnail),
                Err(error) => eprintln!("[flow] video poster unavailable: {error}"),
            }
        }
        progress("uploading");
        if let Some(alias) = request.alias.as_ref() {
            match client.resolve_alias(alias) {
                Ok(found) => request.asset_id = Some(found.asset_id),
                Err(ClientError::NotFound { .. }) => {}
                Err(error) => return Err(format!("resolve publish alias: {error}")),
            }
        }
        client
            .publish_artifact(request)
            .map_err(|error| format!("publish asset: {error}"))
    })();
    if let Ok(published) = &result {
        origins.insert(*published.artifact_blob.as_bytes(), (published.asset_id, published.revision));
    }
    result
}

fn plain_thumbnail(thumbnail: makepad_flow::engine::results::ResultThumb) -> PublishThumbnail {
    PublishThumbnail::plain(thumbnail.png, ThumbnailMedia::Png, thumbnail.width, thumbnail.height)
}

fn connected_client<'a>(
    config: &AssetStoreConfig,
    slot: &'a mut Option<AssetClient>,
) -> Result<&'a mut AssetClient, String> {
    // A store that moves: this command talks to where it is now.
    let located;
    let config = match &config.locate {
        None => config,
        Some(locate) => {
            let (endpoints, server_id, token) = (locate.0)()?;
            if slot.as_ref().is_some_and(|client| client.endpoints() != endpoints) {
                *slot = None;
            }
            let mut now = config.clone();
            now.endpoints = Some(endpoints);
            now.server_id = Some(server_id);
            now.token = Some(token);
            located = now;
            &located
        }
    };
    if slot.is_none() {
        let mut client_config = ClientConfig::new(config.cache_dir.clone());
        client_config.token = config.token.clone();
        let connected = if let Some(endpoints) = config.endpoints {
            AssetClient::connect(client_config, endpoints, config.server_id)
                .map_err(|error| format!("connect to asset server: {error}"))?
        } else {
            if config.discovery_wait_ms == 0 {
                return Err("asset store unavailable: no asset server discovered on this LAN".to_string());
            }
            let listener = DiscoveryListener::start(
                config.discovery_port,
                DISCOVERY_TTL_MS,
                makepad_asset_client::util::now_ms,
            )
            .map_err(|error| format!("start asset server discovery: {error}"))?;
            let deadline = Instant::now() + Duration::from_millis(config.discovery_wait_ms);
            let discovered = loop {
                if let Some(server) =
                    listener.pick(content_client_caps(), makepad_asset_client::util::now_ms())
                {
                    break server;
                }
                if Instant::now() >= deadline {
                    return Err("asset store unavailable: no asset server discovered on this LAN".to_string());
                }
                std::thread::sleep(Duration::from_millis(25));
            };
            AssetClient::connect_discovered(client_config, &discovered)
                .map_err(|error| format!("connect to discovered asset server: {error}"))?
        };
        *slot = Some(connected);
    }
    Ok(slot.as_mut().unwrap())
}

fn read_asset(
    client: &mut AssetClient,
    id: AssetId,
    revision: Option<AssetRevisionId>,
    thumbnail: bool,
    origins: &mut HashMap<[u8; 32], (AssetId, AssetRevisionId)>,
) -> Result<ResultBytes, String> {
    let revision = revision_or_head(client, id, revision)?;
    let manifest = client.fetch_asset_manifest(&revision).map_err(|e| format!("read asset manifest: {e}"))?;
    if thumbnail {
        return read_manifest_thumbnail(client, &manifest);
    }
    let (blob, len, content_type) = {
        let file = manifest.files.iter().min_by_key(|file| {
            let priority = match file.role {
                FileRole::RenderGlb | FileRole::Splat | FileRole::Video | FileRole::Audio | FileRole::Texture => 0,
                FileRole::Source => 1,
                _ => 2,
            };
            (priority, file.lod)
        }).ok_or("asset content not found")?;
        let content_type = match file.media {
            MediaType::Png => "image/png", MediaType::Jpeg => "image/jpeg",
            MediaType::Glb => "model/gltf-binary", MediaType::Ply => "application/ply",
            MediaType::Wav => "audio/wav", MediaType::Ogg => "audio/ogg", MediaType::Mp3 => "audio/mpeg",
            MediaType::Mp4 => "video/mp4", MediaType::Text => "text/plain; charset=utf-8",
            MediaType::Json => "application/json", MediaType::Bin => "application/octet-stream",
        };
        (file.blob, file.byte_len, content_type)
    };
    let bytes = client.fetch_blob_bytes(&blob, Some(len)).map_err(|e| format!("fetch asset content: {e}"))?;
    origins.insert(*blob.as_bytes(), (id, revision));
    Ok(ResultBytes { content_type: content_type.to_string(), bytes })
}

const MAX_LEGACY_VIDEO_POSTER_BYTES: u64 = 64 * 1024 * 1024;

fn read_manifest_thumbnail(
    client: &mut AssetClient,
    manifest: &makepad_asset_data::AssetManifest,
) -> Result<ResultBytes, String> {
    let thumb = manifest.thumbnail.as_ref().ok_or("asset thumbnail not found")?;
    let bytes = client
        .fetch_blob_bytes(&thumb.blob, Some(thumb.byte_len))
        .map_err(|e| format!("fetch asset thumbnail: {e}"))?;

    // Older Flow revisions stored a solid tile for videos. Only fetch the
    // original when its declared size is bounded; card previews must never
    // turn into an unbounded video download.
    if bytes == placeholder_thumbnail()?.png {
        if let Some(video) = manifest
            .files
            .iter()
            .find(|file| file.media == MediaType::Mp4 && file.byte_len <= MAX_LEGACY_VIDEO_POSTER_BYTES)
        {
            if let Ok(original) = client.fetch_blob_bytes(&video.blob, Some(video.byte_len)) {
                if let Ok(poster) = video_thumbnail(&original) {
                    return Ok(ResultBytes { content_type: "image/png".into(), bytes: poster.png });
                }
            }
        }
    }

    let media = match thumb.media {
        ThumbnailMedia::Png => ResultMedia::Png,
        ThumbnailMedia::Jpeg => ResultMedia::Jpeg,
    };
    match image_thumbnail(&bytes, media) {
        Ok(resized) => Ok(ResultBytes { content_type: "image/png".into(), bytes: resized.png }),
        // Oversized/unsupported legacy images must not bypass the worker's
        // limit and become full-resolution textures in the scrolling list.
        Err(_) => Ok(ResultBytes {
            content_type: "image/png".into(),
            bytes: placeholder_thumbnail()?.png,
        }),
    }
}

const MAX_TEXT_PREVIEW_BYTES: u64 = 16 * 1024;

fn read_asset_preview(client: &mut AssetClient, id: AssetId, revision: Option<AssetRevisionId>) -> Result<ResultBytes, String> {
    let revision = revision_or_head(client, id, revision)?;
    let manifest = client.fetch_asset_manifest(&revision).map_err(|e| format!("read asset manifest: {e}"))?;

    // Image/video/mesh/audio cards always use the manifest thumbnail. This
    // Legacy video tiles may use the bounded poster backfill above.
    if manifest.kind != AssetKind::Data && manifest.thumbnail.is_some() {
        return read_manifest_thumbnail(client, &manifest);
    }

    let file = manifest.files.iter().min_by_key(|file| {
        let priority = match file.role {
            FileRole::RenderGlb | FileRole::Splat | FileRole::Video | FileRole::Audio | FileRole::Texture => 0,
            FileRole::Source => 1,
            _ => 2,
        };
        (priority, file.lod)
    }).ok_or("asset content not found")?;
    let content_type = media_content_type(file.media);
    match file.media {
        MediaType::Text | MediaType::Json if file.byte_len <= MAX_TEXT_PREVIEW_BYTES => {
            let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len))
                .map_err(|e| format!("fetch asset preview: {e}"))?;
            Ok(ResultBytes { content_type: content_type.into(), bytes })
        }
        // The blob API is exact-length, so refusing oversized text here is
        // safer than accidentally reading an unbounded source into the UI.
        MediaType::Text | MediaType::Json => Ok(preview_fallback(content_type, file.byte_len)),
        _ => Ok(preview_fallback(content_type, file.byte_len)),
    }
}

fn media_content_type(media: MediaType) -> &'static str {
    match media {
        MediaType::Png => "image/png", MediaType::Jpeg => "image/jpeg",
        MediaType::Glb => "model/gltf-binary", MediaType::Ply => "application/ply",
        MediaType::Wav => "audio/wav", MediaType::Ogg => "audio/ogg", MediaType::Mp3 => "audio/mpeg",
        MediaType::Mp4 => "video/mp4", MediaType::Text => "text/plain; charset=utf-8",
        MediaType::Json => "application/json", MediaType::Bin => "application/octet-stream",
    }
}

fn preview_fallback(content_type: &str, byte_len: u64) -> ResultBytes {
    let label = match content_type {
        "application/octet-stream" => "Binary data",
        "model/gltf-binary" | "application/ply" => "3D model",
        value if value.starts_with("audio/") => "Audio",
        value if value.starts_with("video/") => "Video",
        value if value.starts_with("image/") => "Image",
        _ => "Preview unavailable",
    };
    ResultBytes {
        content_type: "text/plain; charset=utf-8".into(),
        bytes: format!("{label} · {byte_len} bytes").into_bytes(),
    }
}

#[derive(Clone)]
struct AssetPageCursor {
    query: String,
    namespace: Option<String>,
    limit: u32,
    primary: Option<makepad_asset_client::PageCursor>,
    tagged: Option<makepad_asset_client::PageCursor>,
}

fn list_assets(client: &AssetClient, query: &AssetListQuery, pages: &mut HashMap<String, (Instant, AssetPageCursor)>) -> Result<AssetsResponse, String> {
    let limit = query.limit.clamp(1, 100);
    pages.retain(|_, (created, _)| created.elapsed() < Duration::from_secs(600));
    let saved = query.cursor.as_ref().map(|cursor| pages.get(cursor).map(|(_, saved)| saved.clone())
        .ok_or_else(|| "invalid asset cursor; refresh the library".to_string())).transpose()?;
    if saved.as_ref().is_some_and(|saved| saved.query != query.text || saved.namespace != query.namespace || saved.limit != limit) {
        return Err("invalid asset cursor for this query".to_string());
    }
    let make_query = |namespace: Option<String>, tag: Option<String>| {
        // Each source contributes one bounded page; merged pages can contain
        // up to twice the requested limit when flow-tagged imports overlap.
        let mut value = if query.text.trim().is_empty() {
            let mut value = CatalogQuery::browse(limit);
            value.newest = true;
            value
        } else {
            CatalogQuery::text(query.text.trim(), limit)
        };
        value.namespace = namespace;
        value.tag = tag;
        value
    };
    let mut hits = Vec::new();
    let mut seen = HashSet::new();
    let mut next = AssetPageCursor { query: query.text.clone(), namespace: query.namespace.clone(), limit, primary: None, tagged: None };
    for tagged in [false, true] {
        if tagged && query.namespace.as_deref() != Some("flows") { continue; }
        let cursor = saved.as_ref().and_then(|saved| if tagged { saved.tagged.as_ref() } else { saved.primary.as_ref() });
        if saved.is_some() && cursor.is_none() { continue; }
        let catalog_query = if tagged { make_query(None, Some("flow".into())) } else { make_query(query.namespace.clone(), None) };
        let page = client.catalog_search(&catalog_query, cursor)
            .map_err(|error| format!("browse asset catalog: {error}"))?;
        if tagged { next.tagged = page.next; } else { next.primary = page.next; }
        for hit in page.hits {
            if seen.insert(hit.asset_id) { hits.push(hit); }
        }
    }
    let cursor = if next.primary.is_some() || next.tagged.is_some() {
        // Pagination state remains on the asset worker, preserving the
        // upstream client's typed server binding without expanding URLs.
        if pages.len() >= 256 {
            if let Some(oldest) = pages.iter().min_by_key(|(_, (at, _))| *at).map(|(id, _)| id.clone()) {
                pages.remove(&oldest);
            }
        }
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let id = format!("page-{nonce:x}");
        pages.insert(id.clone(), (Instant::now(), next));
        Some(id)
    } else { None };
    if hits.is_empty() { return Ok(AssetsResponse { assets: Vec::new(), cursor }); }
    let ids = hits
        .iter()
        .map(|hit| format!("'{}'", hex(hit.asset_id.as_bytes())))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT lower(hex(a.asset_id)), a.created_ms, COALESCE(group_concat(CASE WHEN l.kind='tag' THEN l.label END, ','), '') FROM assets a LEFT JOIN search_labels l ON l.asset_id=a.asset_id WHERE lower(hex(a.asset_id)) IN ({ids}) GROUP BY a.asset_id, a.created_ms"
    );
    let metadata = client
        .assets_query(&sql)
        .map_err(|error| format!("read asset creation metadata: {error}"))?;
    let mut by_id = HashMap::new();
    for row in metadata.rows {
        if row.len() == 3 {
            let tags = row[2]
                .split(',')
                .filter(|tag| !tag.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            by_id.insert(row[0].clone(), (row[1].parse::<u64>().unwrap_or(0), tags));
        }
    }
    let mut rows = hits
        .into_iter()
        .map(|hit| flow_asset(hit, &by_id))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        right
            .created_ms
            .cmp(&left.created_ms)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(AssetsResponse { assets: rows, cursor })
}

fn flow_asset(hit: CatalogHit, metadata: &HashMap<String, (u64, Vec<String>)>) -> FlowAsset {
    let key = hex(hit.asset_id.as_bytes());
    let (created_ms, tags) = metadata.get(&key).cloned().unwrap_or((hit.updated_ms, Vec::new()));
    FlowAsset {
        id: hit.asset_id.to_string(),
        alias: hit.alias.map(|alias| alias.to_string()),
        namespace: hit.namespace,
        title: hit.title,
        kind: hit
            .kind
            .map(makepad_asset_client::dto::kind_name)
            .unwrap_or("unknown")
            .to_string(),
        tags,
        created_ms,
    }
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}


// The store's annotation bounds (`apps/apps/engine/asset/store/src/search.rs`): a value
// past one is refused there, so fields are cut to fit.
const MAX_PROVENANCE_BYTES: usize = 4096;
/// Typed manifest provenance parents (`engine/asset/data` limits).
const MAX_MANIFEST_PARENTS: usize = 8;

fn attach_lineage(
    request: &mut PublishRequest,
    lineage: &PublishLineage,
    origins: &HashMap<[u8; 32], (AssetId, AssetRevisionId)>,
) -> Vec<(AssetId, AssetRevisionId)> {
    let mut parents = Vec::new();
    for digest in lineage.sources.iter().filter(|digest| **digest != lineage.value) {
        if let Some(origin) = origins.get(digest) {
            if !parents.contains(origin) {
                parents.push(*origin);
            }
        }
    }
    if let Some(provenance) = request.manifest_provenance.as_mut() {
        // The manifest keeps the nearest few, in canonical order; the text
        // record below keeps every one it has room for.
        let mut revisions: Vec<_> = parents.iter().take(MAX_MANIFEST_PARENTS).map(|(_, revision)| *revision).collect();
        revisions.sort();
        revisions.dedup();
        provenance.parents = revisions;
    }
    if !lineage.provenance.is_empty() {
        let parents: Vec<String> = parents.iter().map(|(asset, revision)| format!("{asset}@{revision}")).collect();
        request.provenance = provenance_text(&lineage.provenance, &parents);
    }
    parents
}

/// The provenance record as JSON within the store's bound: when it does not
/// fit, generator parameters go first, then all but the nearest generator,
/// then the parents past those that fit.
fn provenance_text(fields: &[(String, Json)], parents: &[String]) -> String {
    let render = |fields: &[(String, Json)], parents: &[String]| {
        let mut fields = fields.to_vec();
        if !parents.is_empty() {
            fields.push(("parents".to_string(), Json::Arr(parents.iter().cloned().map(Json::Str).collect())));
        }
        Json::Obj(fields).to_json()
    };
    let text = render(fields, parents);
    if text.len() <= MAX_PROVENANCE_BYTES {
        return text;
    }
    let mut lean = fields.to_vec();
    for (name, value) in &mut lean {
        if name == "generators" {
            if let Json::Arr(records) = value {
                for record in records.iter_mut() {
                    if let Json::Obj(record) = record {
                        record.retain(|(key, _)| key != "params");
                    }
                }
            }
        }
    }
    let text = render(&lean, parents);
    if text.len() <= MAX_PROVENANCE_BYTES {
        return text;
    }
    for (name, value) in &mut lean {
        if name == "generators" {
            if let Json::Arr(records) = value {
                records.truncate(1);
            }
        }
    }
    let mut kept = parents.len();
    loop {
        let text = render(&lean, &parents[..kept]);
        if text.len() <= MAX_PROVENANCE_BYTES || kept == 0 {
            return cut(&text, MAX_PROVENANCE_BYTES);
        }
        kept -= 1;
    }
}

fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}


// ---- the ResultStore seam ---------------------------------------------------

fn asset_kind(kind: ResultKind) -> AssetKind {
    match kind {
        ResultKind::Texture => AssetKind::Texture,
        ResultKind::Mesh => AssetKind::Mesh,
        ResultKind::Character => AssetKind::Character,
        ResultKind::Audio => AssetKind::Audio,
        ResultKind::Video => AssetKind::Video,
        ResultKind::World => AssetKind::World,
        ResultKind::Data => AssetKind::Data,
        ResultKind::Flow => AssetKind::Flow,
    }
}

fn result_kind(kind: AssetKind) -> ResultKind {
    match kind {
        AssetKind::Texture => ResultKind::Texture,
        AssetKind::Mesh => ResultKind::Mesh,
        AssetKind::Character => ResultKind::Character,
        AssetKind::Audio => ResultKind::Audio,
        AssetKind::Video => ResultKind::Video,
        AssetKind::World => ResultKind::World,
        AssetKind::Flow => ResultKind::Flow,
        _ => ResultKind::Data,
    }
}

fn file_role(role: ResultRole) -> FileRole {
    match role {
        ResultRole::Texture => FileRole::Texture,
        ResultRole::Depth => FileRole::Depth,
        ResultRole::Video => FileRole::Video,
        ResultRole::Audio => FileRole::Audio,
        ResultRole::RenderGlb => FileRole::RenderGlb,
        ResultRole::Splat => FileRole::Splat,
        ResultRole::Source => FileRole::Source,
    }
}

fn media_type(media: ResultMedia) -> MediaType {
    match media {
        ResultMedia::Png => MediaType::Png,
        ResultMedia::Jpeg => MediaType::Jpeg,
        ResultMedia::Glb => MediaType::Glb,
        ResultMedia::Ply => MediaType::Ply,
        ResultMedia::Wav => MediaType::Wav,
        ResultMedia::Ogg => MediaType::Ogg,
        ResultMedia::Mp3 => MediaType::Mp3,
        ResultMedia::Mp4 => MediaType::Mp4,
        ResultMedia::Text => MediaType::Text,
        ResultMedia::Json => MediaType::Json,
        ResultMedia::Bin => MediaType::Bin,
    }
}

/// The store's publish request for a result: the engine's description of the
/// run's value as the asset library files it.
fn to_request(files: ResultFiles) -> Result<PublishRequest, String> {
    let [file] = <[_; 1]>::try_from(files.files).map_err(|_| "a result takes exactly one file".to_string())?;
    let thumbnail = match files.thumbnail {
        Some(thumbnail) => plain_thumbnail(thumbnail),
        None => plain_thumbnail(placeholder_thumbnail()?),
    };
    let artifact = PublishFile {
        bytes: file.bytes,
        media: media_type(file.media),
        role: file_role(file.role),
        media_millis: file.media_millis,
        dims: file.dims,
    };
    let mut request = PublishRequest::new(files.namespace, asset_kind(files.kind), files.title, artifact, thumbnail);
    request.categories = files.category.into_iter().collect();
    if let Some(stats) = files.mesh_stats {
        request.stats = makepad_asset_client::PublishStats {
            triangles: stats.triangles,
            vertices: stats.vertices,
            joints: stats.joints,
            clips: stats.clips,
        };
    }
    request.description = files.description;
    request.alias = files.alias
        .map(|alias| AssetAlias::new(alias).map_err(|error| format!("publish alias: {error}")))
        .transpose()?;
    request.tags = files.tags;
    request.creator = files.creator;
    request.artist = files.artist;
    request.generator = files.generator;
    request.model = files.model;
    // The hub node that ran the job (`node 42`): the annotation's backend.
    if !files.hub.is_empty() {
        request.backend = files.hub;
    }
    request.prompt = files.prompt;
    request.manifest_provenance = files.seed.map(|seed| PublishProvenance {
        generator: request.generator.clone(),
        model: request.model.clone(),
        version: files.generator_version,
        seed,
        parents: Vec::new(),
        params_digest: files.digest,
    });
    Ok(request)
}

/// The revision a reference names exactly, or `None` for the newest
/// publication (HEAD).
fn ref_revision(result: &ResultRef) -> Result<Option<AssetRevisionId>, String> {
    match result.revision.as_deref() {
        None => Ok(None),
        Some(revision) => revision.parse::<AssetRevisionId>().map(Some).map_err(|_| format!("`{revision}` is not a revision id")),
    }
}

/// `revision` when the reference named one, else the asset's newest
/// published revision.
fn revision_or_head(client: &mut AssetClient, id: AssetId, revision: Option<AssetRevisionId>) -> Result<AssetRevisionId, String> {
    if let Some(revision) = revision {
        return Ok(revision);
    }
    let detail = client.asset_detail(&id).map_err(|e| format!("read asset: {e}"))?;
    Ok(detail.latest_published().ok_or("asset has no published revision")?.revision)
}

fn parse_id(id: &str) -> Result<AssetId, String> {
    id.parse::<AssetId>().map_err(|_| "invalid asset id".to_string())
}

/// The asset a reference names: its id, or what its alias points at.
fn resolve_id(client: &mut AssetClient, result: &ResultRef) -> Result<AssetId, String> {
    if !result.id.is_empty() {
        return parse_id(&result.id);
    }
    let alias = result.alias.as_ref().ok_or_else(|| "invalid result reference".to_string())?;
    let alias = AssetAlias::new(alias.clone()).map_err(|_| "invalid asset alias".to_string())?;
    match client.resolve_alias(&alias) {
        Ok(found) => Ok(found.asset_id),
        Err(ClientError::NotFound { .. }) => Err("asset alias not found".to_string()),
        Err(error) => Err(format!("resolve asset alias: {error}")),
    }
}

/// The stored product a fixed-seed run left under its reuse alias (see
/// `makepad_gen::digest::reuse_alias`), when its manifest carries this digest.
fn find_by_digest(client: &mut AssetClient, namespace: &str, digest_hex: &str) -> Option<ResultRef> {
    if digest_hex.len() != 64 || !digest_hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let digest = {
        let mut out = [0u8; 32];
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&digest_hex[index * 2..index * 2 + 2], 16).ok()?;
        }
        out
    };
    let alias = AssetAlias::new(format!("{namespace}/gen-{}", &digest_hex.to_ascii_lowercase()[..32])).ok()?;
    let found = client.resolve_alias(&alias).ok()?;
    let manifest = client.fetch_asset_manifest(&found.head_revision).ok()?;
    let same = manifest.provenance.as_ref().is_some_and(|provenance| provenance.params_digest == Some(digest));
    same.then(|| ResultRef {
        id: found.asset_id.to_string(),
        revision: Some(found.head_revision.to_string()),
        alias: Some(alias.to_string()),
    })
}

fn info_of(client: &mut AssetClient, result: &ResultRef) -> Option<ResultInfo> {
    let id = resolve_id(client, result).ok()?;
    let detail = client.asset_detail(&id).ok()?;
    let revision = detail.latest_published()?.revision;
    let manifest = client.fetch_asset_manifest(&revision).ok()?;
    let file = manifest.files.first()?;
    let key = hex(id.as_bytes());
    let title = client
        .assets_query(&format!("SELECT title FROM search_annotations WHERE lower(hex(asset_id))='{key}'"))
        .ok()
        .and_then(|rows| rows.rows.into_iter().next().and_then(|row| row.into_iter().next()))
        .unwrap_or_default();
    let tags = client
        .assets_query(&format!("SELECT label FROM search_labels WHERE kind='tag' AND lower(hex(asset_id))='{key}'"))
        .ok()
        .map(|rows| rows.rows.into_iter().filter_map(|row| row.into_iter().next()).collect())
        .unwrap_or_default();
    Some(ResultInfo {
        title,
        kind: result_kind(manifest.kind),
        content_type: media_content_type(file.media).to_string(),
        tags,
    })
}

impl ResultStore for AssetWorkerHandle {
    fn archive_outputs(&self) -> bool {
        self.archive_outputs
    }

    fn publish(&self, files: ResultFiles) -> Result<Receiver<PublishEvent>, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(AssetCommand::PublishFiles(files, tx))
            .map_err(|_| "asset worker is not running".to_string())?;
        Ok(rx)
    }

    fn find_by_digest(&self, digest: &str) -> Option<ResultRef> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(AssetCommand::FindDigest(digest.to_string(), tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(15)).ok().flatten()
    }

    fn list(&self, query: ResultQuery) -> Result<ResultPage, String> {
        let page = self.list_page(AssetListQuery {
            text: query.text,
            namespace: query.namespace,
            limit: query.limit,
            cursor: query.cursor,
        })?;
        let items = page
            .assets
            .into_iter()
            .filter(|asset| match query.kind {
                None => true,
                Some(ResultKind::Flow) => asset.kind == "flow" || asset.tags.iter().any(|tag| tag == PRESET_TAG),
                Some(kind) => asset.kind == kind.name(),
            })
            .collect();
        Ok(ResultPage { items, cursor: page.cursor })
    }

    fn read(&self, result: &ResultRef) -> Result<ResultBytes, String> {
        self.read_ref(result, ReadMode::Content, Duration::from_secs(60))
    }

    fn preview(&self, result: &ResultRef) -> Result<ResultBytes, String> {
        self.read_ref(result, ReadMode::Preview, Duration::from_secs(60))
    }

    fn thumbnail(&self, result: &ResultRef) -> Result<Option<ResultBytes>, String> {
        self.read_ref(result, ReadMode::Thumbnail, Duration::from_secs(15)).map(Some)
    }

    fn info(&self, result: &ResultRef) -> Option<ResultInfo> {
        let (tx, rx) = mpsc::channel();
        self.tx.send(AssetCommand::Info(result.clone(), tx)).ok()?;
        rx.recv_timeout(Duration::from_secs(15)).ok().flatten()
    }

    fn note_origin(&self, input_digest: [u8; 32], result: &ResultRef) {
        let _ = self.tx.send(AssetCommand::NoteOriginRef(input_digest, result.clone()));
    }
}

impl AssetWorkerHandle {
    fn read_ref(&self, result: &ResultRef, mode: ReadMode, wait: Duration) -> Result<ResultBytes, String> {
        let (tx, rx) = mpsc::channel();
        self.tx
            .send(AssetCommand::ReadRef(result.clone(), mode, tx))
            .map_err(|_| "asset worker is not running".to_string())?;
        rx.recv_timeout(wait).map_err(|_| "asset content request timed out".to_string())?
    }

    /// This worker as the engine's result store.
    pub fn store(&self) -> Arc<dyn ResultStore> {
        Arc::new(self.clone())
    }
}

impl AssetWorker {
    /// The running worker as the engine's result store.
    pub fn store(&self) -> Arc<dyn ResultStore> {
        self.handle.store()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_flow::engine::executors::publish::PUBLISH_GENERATOR;

    fn request() -> PublishRequest {
        let file = PublishFile { bytes: b"{}".to_vec(), media: MediaType::Json, role: FileRole::Source, media_millis: 0, dims: None };
        PublishRequest::new("flows", AssetKind::Data, "t", file, plain_thumbnail(placeholder_thumbnail().unwrap()))
    }

    #[test]
    fn library_sources_become_parents_and_provenance() {
        let asset = AssetId::from_bytes([3u8; 16]);
        let revision = AssetRevisionId::hash_of(b"revision");
        let library = [7u8; 32];
        let own = [9u8; 32];
        let origins = HashMap::from([(library, (asset, revision)), (own, (asset, AssetRevisionId::hash_of(b"own")))]);
        let mut with_provenance = request();
        with_provenance.manifest_provenance = Some(PublishProvenance {
            generator: PUBLISH_GENERATOR.into(),
            model: "flux1-dev".into(),
            version: "0".into(),
            seed: 4,
            parents: Vec::new(),
            params_digest: None,
        });
        let lineage = PublishLineage {
            value: own,
            sources: vec![own, [1u8; 32], library, library],
            provenance: vec![("flow".to_string(), Json::Str("f".into()))],
        };
        let parents = attach_lineage(&mut with_provenance, &lineage, &origins);
        assert_eq!(parents, [(asset, revision)]);
        assert_eq!(with_provenance.manifest_provenance.as_ref().unwrap().parents, [revision]);
        assert_eq!(with_provenance.provenance, format!(r#"{{"flow":"f","parents":["{asset}@{revision}"]}}"#));
    }

    #[test]
    fn provenance_text_sheds_detail_to_fit_the_store_bound() {
        let params = Json::Obj(vec![("negative".to_string(), Json::Str("x".repeat(3000)))]);
        let record = |node: &str| Json::Obj(vec![("node".to_string(), Json::Str(node.into())), ("params".to_string(), params.clone())]);
        let fields = vec![
            ("flow".to_string(), Json::Str("f".into())),
            ("generators".to_string(), Json::Arr(vec![record("video"), record("image")])),
        ];
        let text = provenance_text(&fields, &["p".repeat(40)]);
        assert!(text.len() <= MAX_PROVENANCE_BYTES);
        assert_eq!(text, format!(r#"{{"flow":"f","generators":[{{"node":"video"}},{{"node":"image"}}],"parents":["{}"]}}"#, "p".repeat(40)));
        let parents: Vec<String> = (0..200).map(|index| format!("{index:040}")).collect();
        let text = provenance_text(&fields, &parents);
        assert!(text.len() <= MAX_PROVENANCE_BYTES && text.contains(r#""generators":[{"node":"video"}]"#), "{text}");
        assert!(makepad_strict_json::parse(text.as_bytes()).is_ok());
    }

    #[test]
    fn binary_preview_refuses_payload_and_reports_declared_size() {
        let preview = preview_fallback("application/octet-stream", 9 * 1024 * 1024);
        assert_eq!(preview.content_type, "text/plain; charset=utf-8");
        assert_eq!(preview.bytes, "Binary data · 9437184 bytes".as_bytes());
    }


    fn audio_files() -> ResultFiles {
        use makepad_flow::engine::results::{ResultFile, ResultThumb};
        ResultFiles {
            kind: ResultKind::Audio,
            namespace: "flows".into(),
            title: "Beat".into(),
            description: "d".into(),
            alias: Some("flows/beat".into()),
            category: Some("music".into()),
            tags: vec!["flow".into()],
            files: vec![ResultFile { role: ResultRole::Audio, media: ResultMedia::Wav, bytes: vec![1, 2, 3], media_millis: 1500, dims: None }],
            thumbnail: Some(ResultThumb { png: vec![9], width: 256, height: 256 }),
            mesh_stats: None,
            creator: "flow:x".into(),
            artist: String::new(),
            generator: "makepad-flow".into(),
            model: "m".into(),
            hub: String::new(),
            prompt: "p".into(),
            seed: Some(5),
            digest: Some([4u8; 32]),
            generator_version: "0.1.0".into(),
            provenance: Vec::new(),
            value_digest: [1u8; 32],
            sources: Vec::new(),
        }
    }

    #[test]
    fn a_result_becomes_the_stores_publish_request() {
        let request = to_request(audio_files()).unwrap();
        assert_eq!(request.backend, "");
        assert_eq!((request.kind, request.artifact.media, request.artifact.role), (AssetKind::Audio, MediaType::Wav, FileRole::Audio));
        assert_eq!(request.alias.as_ref().map(ToString::to_string).as_deref(), Some("flows/beat"));
        assert_eq!(request.categories, ["music"]);
        let provenance = request.manifest_provenance.unwrap();
        assert_eq!((provenance.model.as_str(), provenance.seed, provenance.params_digest), ("m", 5, Some([4u8; 32])));
    }

    #[test]
    fn publish_files_carries_hub_into_asset_worker_backend() {
        let files = ResultFiles { hub: "node 42".into(), model: "fasth3-4step-q4-24g".into(), ..audio_files() };
        let request = to_request(files).unwrap();
        assert_eq!(request.backend, "node 42");
        assert_eq!(request.model, "fasth3-4step-q4-24g");
        assert_eq!(request.generator, "makepad-flow");
    }
}

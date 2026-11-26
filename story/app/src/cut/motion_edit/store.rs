//! An opened graphic's library side: reading a Motion asset's source, and
//! autosaving it as a new `AssetKind::Motion` revision (it holds
//! Source/Text `motion.splash`, a thumbnail, and every revision the
//! document names as its dependencies). One long-lived worker with its own
//! asset client; the UI thread only `try_send`s jobs and `try_recv`s
//! replies, both bounded; a full queue is retried on a later pump.

use makepad_asset_connection as asset_connection;
use makepad_asset_connection::redact;
use makepad_asset_client::resolver::{select_file, TierPreference};
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionRef, DeviceTier, FileRole, MediaType, ThumbnailMedia};
use makepad_widgets::*;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;

const QUEUE: usize = 4;
const NAMESPACE: &str = "stage";
/// A Motion document's source; far above any document, refused past it.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

pub enum MotionJob {
    Open { asset: AssetRevisionRef },
    /// An image or SVG a document shows (its bytes, and `png`/`jpg`/`svg`).
    Media { media: makepad_motion::Media },
    Publish { serial: u64, asset: Option<AssetId>, title: String, source: String, thumbnail: (usize, usize, Vec<u32>) },
    /// The next revision of a library document whose loaded revision
    /// changed only in values and in the revisions it names (the edit's
    /// panel): its dependencies are the `asset(…)`s it writes, nothing is
    /// loaded again; its picture is the one revision `from` has.
    Revise { serial: u64, from: AssetRevisionRef, title: String, source: String },
}

pub enum MotionReply {
    Opened { asset: AssetRevisionRef, result: Result<(String, String), String> },
    Media { media: makepad_motion::Media, result: Result<(Vec<u8>, &'static str), String> },
    Published { serial: u64, source: String, result: Result<AssetRevisionRef, String> },
}

pub struct MotionStore {
    jobs: Option<SyncSender<MotionJob>>,
    replies: Receiver<MotionReply>,
    pub error: Option<String>,
}

impl MotionStore {
    pub fn new(cx: &mut Cx) -> Self {
        let (job_tx, job_rx) = sync_channel(QUEUE);
        let (reply_tx, reply_rx) = sync_channel(QUEUE);
        let options = ThreadOptions { name: Some(Arc::from("stage-motion-store")), stack_size: None, priority: CxThreadPriority::Utility };
        let mut store = Self { jobs: None, replies: reply_rx, error: None };
        match cx.thread_spawner().spawn_worker(options, move || worker(job_rx, reply_tx)) {
            Ok(handle) => {
                handle.detach();
                store.jobs = Some(job_tx);
            }
            Err(error) => store.error = Some(format!("motion store worker unavailable: {error:?}")),
        }
        store
    }

    pub fn try_send(&self, job: MotionJob) -> Result<(), MotionJob> {
        let Some(jobs) = &self.jobs else { return Err(job) };
        jobs.try_send(job).map_err(|error| match error {
            TrySendError::Full(job) | TrySendError::Disconnected(job) => job,
        })
    }

    pub fn try_recv(&self) -> Option<MotionReply> {
        self.replies.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.jobs.take();
    }
}

fn worker(jobs: Receiver<MotionJob>, replies: SyncSender<MotionReply>) {
    let origin = asset_connection::origin();
    let mut client: Option<AssetClient> = None;
    while let Ok(job) = jobs.recv() {
        if client.as_ref().is_some_and(|client| !asset_connection::is_current(client.endpoints())) {
            client = None;
        }
        if client.is_none() {
            match asset_connection::connect(&origin) {
                Ok(connected) => client = Some(connected),
                Err(error) => {
                    let reply = match job {
                        MotionJob::Open { asset } => MotionReply::Opened { asset, result: Err(error) },
                        MotionJob::Media { media } => MotionReply::Media { media, result: Err(error) },
                        MotionJob::Publish { serial, source, .. } | MotionJob::Revise { serial, source, .. } => MotionReply::Published { serial, source, result: Err(error) },
                    };
                    if replies.send(reply).is_err() {
                        break;
                    }
                    continue;
                }
            }
        }
        let client_ref = client.as_mut().unwrap();
        let reply = match job {
            MotionJob::Open { asset } => MotionReply::Opened { asset, result: open(client_ref, &asset) },
            MotionJob::Media { media } => {
                use makepad_story_edit_render::MediaResolver;
                let result = match &media {
                    makepad_motion::Media::Asset(asset) => {
                        let mut store = makepad_story_edit_render::StoreMedia::new(client_ref.lane_clone());
                        store.asset_bytes(&makepad_story_cut::AssetRef { id: asset.id.clone(), rev: asset.rev.clone() })
                    }
                    _ => Err("only library assets are fetched".into()),
                };
                MotionReply::Media { media, result }
            }
            MotionJob::Publish { serial, asset, title, source, thumbnail } => {
                let result = publish(client_ref, asset, &title, &source, thumbnail);
                MotionReply::Published { serial, source, result }
            }
            MotionJob::Revise { serial, from, title, source } => {
                let result = revise(client_ref, from, &title, &source);
                MotionReply::Published { serial, source, result }
            }
        };
        if let MotionReply::Opened { result: Err(_), .. } | MotionReply::Published { result: Err(_), .. } = &reply {
            // Reconnect next time: the connection may be what failed.
            client = None;
        }
        if replies.send(reply).is_err() {
            break;
        }
    }
}

/// A Motion asset's source and title.
fn open(client: &mut AssetClient, reference: &AssetRevisionRef) -> Result<(String, String), String> {
    let manifest = client.resolve_ref(reference).map_err(redact)?;
    if manifest.kind != AssetKind::Motion {
        return Err("that asset is not a motion graphic".into());
    }
    let file = select_file(&manifest, FileRole::Source, TierPreference::PreferWithAnyFallback(DeviceTier::High), 0).map_err(redact)?;
    if file.media != MediaType::Text || file.byte_len > MAX_SOURCE_BYTES {
        return Err("the graphic's source is not text of a readable size".into());
    }
    let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(redact)?;
    let source = String::from_utf8(bytes).map_err(|_| "the graphic's source is not UTF-8".to_string())?;
    // The document's own `label` names it.
    let title = makepad_motion::patch::label_of(&source).unwrap_or_else(|| "Graphic".to_string());
    Ok((source, title))
}

/// The next revision of `asset` (a module or a graphic) without loading it:
/// its dependencies are the revisions its text names (`asset("id", "rev")`).
fn revise(client: &mut AssetClient, from: AssetRevisionRef, title: &str, source: &str) -> Result<AssetRevisionRef, String> {
    let asset = from.asset_id;
    let mut dependencies = Vec::new();
    for part in source.split("asset(\"").skip(1) {
        let mut quoted = part.split('"');
        let (Some(id), _, Some(rev)) = (quoted.next(), quoted.next(), quoted.next()) else { continue };
        let (Ok(asset_id), Ok(revision)) = (id.parse(), rev.parse()) else { continue };
        let reference = AssetRevisionRef { asset_id, revision };
        if !dependencies.contains(&reference) {
            dependencies.push(reference);
        }
    }
    // The revision before's picture (a plain card when it has none).
    let before = client.resolve_ref(&from).ok().and_then(|m| m.thumbnail).and_then(|t| {
        let bytes = client.fetch_blob_bytes(&t.blob, Some(t.byte_len)).ok()?;
        Some(PublishThumbnail { bytes, media: t.media, width: t.width, height: t.height, views: t.views })
    });
    let thumbnail = match before {
        Some(t) => t,
        None => {
            // (The store's smallest card.)
            let (w, h) = (448usize, 256usize);
            let mut bytes = Vec::new();
            jpeg_encoder::Encoder::new(&mut bytes, 85).encode(&vec![0x16u8; w * h * 4], w as u16, h as u16, jpeg_encoder::ColorType::Bgra).map_err(|error| format!("thumbnail: {error}"))?;
            PublishThumbnail::plain(bytes, ThumbnailMedia::Jpeg, w as u32, h as u32)
        }
    };
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)];
    let mut bundle = PublishBundle::new(NAMESPACE, AssetKind::Motion, title.trim().to_string(), files, thumbnail, PublishRights::generated_cc0());
    bundle.asset_id = Some(asset);
    bundle.dependencies = dependencies;
    bundle.generator = "stage-edit-panel".into();
    bundle.tags = if makepad_motion::modules::is_module(source) { vec!["motion".into(), makepad_motion::modules::MODULE_TAG.into()] } else { Vec::new() };
    let published = client.publish_bundle(&bundle).map_err(redact)?;
    Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
}

/// Publish `source` as a new revision of `asset` (a new asset when none),
/// its dependencies every revision the document names.
fn publish(client: &mut AssetClient, asset: Option<AssetId>, title: &str, source: &str, (width, height, pixels): (usize, usize, Vec<u32>)) -> Result<AssetRevisionRef, String> {
    // A library module (a graphic's imported code, edited in place): its
    // next revision, tagged as a module, after it loads as one.
    if makepad_motion::modules::is_module(source) {
        makepad_motion::modules::check(source, Default::default()).map_err(|errors| errors.into_iter().take(2).collect::<Vec<_>>().join("; "))?;
        let mut dependencies = Vec::new();
        for dep in makepad_motion::modules::asset_imports(source) {
            let reference = AssetRevisionRef {
                asset_id: dep.id.parse().map_err(|_| format!("`{}` is not an asset id", dep.id))?,
                revision: dep.rev.parse().map_err(|_| format!("`{}` is not a revision id", dep.rev))?,
            };
            if !dependencies.contains(&reference) {
                dependencies.push(reference);
            }
        }
        let (w, h) = (448usize, 256usize);
        let bgra = vec![0x16u8; w * h * 4];
        let mut bytes = Vec::new();
        jpeg_encoder::Encoder::new(&mut bytes, 85).encode(&bgra, w as u16, h as u16, jpeg_encoder::ColorType::Bgra).map_err(|error| format!("thumbnail: {error}"))?;
        let thumbnail = PublishThumbnail::plain(bytes, ThumbnailMedia::Jpeg, w as u32, h as u32);
        let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)];
        let title = if title.trim().is_empty() { "Module".to_string() } else { title.trim().to_string() };
        let mut bundle = PublishBundle::new(NAMESPACE, AssetKind::Motion, title, files, thumbnail, PublishRights::generated_cc0());
        bundle.asset_id = asset;
        bundle.dependencies = dependencies;
        bundle.generator = "stage-motion-edit".into();
        bundle.tags = vec!["motion".into(), makepad_motion::modules::MODULE_TAG.into()];
        let published = client.publish_bundle(&bundle).map_err(redact)?;
        return Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision });
    }
    let doc = makepad_motion::MotionDoc::load(source).map_err(|diags| {
        diags.iter().filter(|diag| diag.is_error()).take(2).map(ToString::to_string).collect::<Vec<_>>().join("; ")
    })?;
    let mut dependencies = Vec::new();
    for dep in doc.dependencies() {
        let reference = AssetRevisionRef {
            asset_id: dep.id.parse().map_err(|_| format!("`{}` is not an asset id", dep.id))?,
            revision: dep.rev.parse().map_err(|_| format!("`{}` is not a revision id", dep.rev))?,
        };
        if !dependencies.contains(&reference) {
            dependencies.push(reference);
        }
    }
    let bgra: Vec<u8> = pixels.iter().flat_map(|pixel| pixel.to_le_bytes()).collect();
    let mut bytes = Vec::new();
    jpeg_encoder::Encoder::new(&mut bytes, 85)
        .encode(&bgra, width as u16, height as u16, jpeg_encoder::ColorType::Bgra)
        .map_err(|error| format!("thumbnail: {error}"))?;
    let thumbnail = PublishThumbnail::plain(bytes, ThumbnailMedia::Jpeg, width as u32, height as u32);
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)];
    let title = if title.trim().is_empty() { "Untitled graphic".to_string() } else { title.trim().to_string() };
    let mut bundle = PublishBundle::new(NAMESPACE, AssetKind::Motion, title, files, thumbnail, PublishRights::generated_cc0());
    bundle.asset_id = asset;
    bundle.dependencies = dependencies;
    bundle.generator = "stage-motion-edit".into();
    let published = client.publish_bundle(&bundle).map_err(redact)?;
    Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
}

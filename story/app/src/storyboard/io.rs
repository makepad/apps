//! The storyboard's disk and library work, on one long-lived thread:
//!
//! - **Drafts**: every board is written, a second after each change, to
//!   `$STAGE_STORYBOARDS` (else `<makepad home>/stage-product/storyboards`)
//!   as `<key>.splash`, the key being the open cut's draft id (its asset
//!   id once saved). A board follows its cut: opening the cut opens it.
//! - **Library**: saving the cut publishes the board beside it as an
//!   `AssetKind::Storyboard` revision whose dependencies are the references
//!   it shows (so GC keeps them), tagged `edit-<asset id>`.
//! - **Pictures**: a pasted or dropped picture becomes an image asset (PNG
//!   as it came, provenance recorded) that the board then names exactly.

use makepad_asset_connection as asset_connection;
use makepad_asset_connection::redact;
use makepad_asset_client::{AssetClient, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetId, AssetKind, AssetRevisionRef, DerivativePolicy, FileRole, MediaType, Redistribution, ThumbnailMedia};
use makepad_widgets::makepad_platform::thread::SignalToUI;
use makepad_widgets::*;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

const NAMESPACE: &str = "stage";
/// The largest picture taken from a paste or a drop.
const MAX_PICTURE_BYTES: usize = 48 << 20;

pub enum Job {
    Load { key: String },
    Write { key: String, source: String },
    /// Move a board to a new key (an untitled edit saved for the first time).
    Rename { from: String, to: String },
    Publish { key: String, title: String, source: String, dependencies: Vec<AssetRevisionRef>, asset: Option<AssetId>, edit: Option<String> },
    /// A picture's bytes (PNG or JPEG) or a file to import: `ticket`
    /// answers with the new asset.
    Picture { ticket: u64, name: String, bytes: Option<Arc<Vec<u8>>>, path: Option<PathBuf> },
}

pub enum Reply {
    /// `published`: the content key of the source last published from
    /// here and its library revision (`<key>.published`).
    Loaded { key: String, source: Option<String>, published: Option<(String, AssetRevisionRef)> },
    Published { key: String, result: Result<AssetRevisionRef, String> },
    Picture { ticket: u64, result: Result<(AssetRevisionRef, String, bool), String> },
    Failed(String),
}

pub struct Io {
    jobs: Option<Sender<Job>>,
    replies: Receiver<Reply>,
}

pub fn dir() -> PathBuf {
    makepad_app_shell::user_state::dir(Some("STAGE_STORYBOARDS"), "storyboards")
}

/// A key that is a safe file name.
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl Io {
    pub fn new(cx: &mut Cx) -> Result<Self, String> {
        let (job_tx, job_rx) = channel();
        let (reply_tx, reply_rx) = channel();
        let dir = dir();
        let options = ThreadOptions { name: Some(Arc::from("stage-storyboards")), stack_size: Some(4 * 1024 * 1024), priority: CxThreadPriority::Utility };
        let handle = cx.thread_spawner().spawn_worker(options, move || worker(&dir, job_rx, reply_tx)).map_err(|error| format!("{error:?}"))?;
        handle.detach();
        Ok(Self { jobs: Some(job_tx), replies: reply_rx })
    }

    pub fn send(&self, job: Job) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    pub fn try_recv(&self) -> Option<Reply> {
        self.replies.try_recv().ok()
    }

    pub fn stop(&mut self) {
        self.jobs.take();
    }
}

fn worker(dir: &Path, jobs: Receiver<Job>, replies: Sender<Reply>) {
    let mut client: Option<AssetClient> = None;
    let origin = asset_connection::origin();
    let connected = |client: &mut Option<AssetClient>| -> Result<(), String> {
        asset_connection::connected(client, &origin).map(|_| ())
    };
    while let Ok(job) = jobs.recv() {
        let reply = match job {
            Job::Load { key } => {
                let mut source = valid_key(&key).then(|| std::fs::read_to_string(dir.join(format!("{key}.splash"))).ok()).flatten();
                let mut from_library = None;
                // A library edit without a board here: its board from the
                // library (published beside it, tagged with its id).
                if source.is_none() && key.starts_with("ast_") && connected(&mut client).is_ok() {
                    match library_board(client.as_mut().unwrap(), &key) {
                        Ok(Some((found, reference))) => {
                            from_library = Some((content_key(&found), reference));
                            source = Some(found);
                        }
                        Ok(None) => {}
                        Err(error) => {
                            log!("storyboard: the library's board of {key}: {error}");
                            client = None;
                        }
                    }
                }
                let published = from_library.or_else(|| valid_key(&key).then(|| std::fs::read_to_string(dir.join(format!("{key}.published"))).ok()).flatten().and_then(|text| {
                    let mut parts = text.split_whitespace();
                    let (hash, asset, revision) = (parts.next()?.to_string(), parts.next()?, parts.next()?);
                    Some((hash, AssetRevisionRef { asset_id: asset.parse().ok()?, revision: revision.parse().ok()? }))
                }));
                Some(Reply::Loaded { key, source, published })
            }
            Job::Write { key, source } => {
                if !valid_key(&key) {
                    continue;
                }
                let result = std::fs::create_dir_all(dir).and_then(|_| {
                    let temp = dir.join(format!(".{key}.splash.tmp"));
                    std::fs::write(&temp, source.as_bytes())?;
                    std::fs::rename(&temp, dir.join(format!("{key}.splash")))
                });
                result.err().map(|error| Reply::Failed(format!("storyboard: writing {key}: {error}")))
            }
            Job::Rename { from, to } => {
                if !valid_key(&from) || !valid_key(&to) {
                    continue;
                }
                let (a, b) = (dir.join(format!("{from}.splash")), dir.join(format!("{to}.splash")));
                if a.exists() && !b.exists() {
                    std::fs::rename(&a, &b).err().map(|error| Reply::Failed(format!("storyboard: {error}")))
                } else {
                    None
                }
            }
            Job::Publish { key, title, source, dependencies, asset, edit } => {
                let result = connected(&mut client).and_then(|_| publish_board(client.as_mut().unwrap(), &title, &source, dependencies, asset, edit.as_deref()));
                match &result {
                    Ok(reference) if valid_key(&key) => {
                        let _ = std::fs::write(dir.join(format!("{key}.published")), format!("{} {} {}\n", content_key(&source), reference.asset_id, reference.revision));
                    }
                    Ok(_) => {}
                    Err(_) => client = None,
                }
                Some(Reply::Published { key, result })
            }
            Job::Picture { ticket, name, bytes, path } => {
                let bytes = match (bytes, path) {
                    (Some(bytes), _) => Ok(Arc::unwrap_or_clone(bytes)),
                    (None, Some(path)) => std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display())),
                    (None, None) => Err("nothing to import".to_string()),
                };
                let result = bytes.and_then(|bytes| {
                    connected(&mut client)?;
                    publish_picture(client.as_mut().unwrap(), &name, bytes)
                });
                Some(Reply::Picture { ticket, result })
            }
        };
        if let Some(reply) = reply {
            if replies.send(reply).is_err() {
                break;
            }
            SignalToUI::set_ui_signal();
        }
    }
}

/// A source's content key (what was published).
pub fn content_key(source: &str) -> String {
    format!("{:016x}", makepad_story_storyboard::splash::fnv64(source.as_bytes()))
}

/// The tag naming the cut a board was published beside.
fn cut_tag(edit: &str) -> String {
    format!("edit-{}", edit.to_ascii_lowercase()).chars().take(48).collect()
}

/// The newest board published for edit `edit` (tagged `edit-<id>`).
fn library_board(client: &mut AssetClient, edit: &str) -> Result<Option<(String, AssetRevisionRef)>, String> {
    let mut query = makepad_asset_client::api::CatalogQuery::browse(4);
    query.kind = Some(AssetKind::Storyboard);
    query.newest = true;
    query.tag = Some(cut_tag(edit));
    let page = client.catalog_search(&query, None).map_err(redact)?;
    let Some(hit) = page.hits.into_iter().next() else { return Ok(None) };
    let detail = client.asset_detail(&hit.asset_id).map_err(redact)?;
    let Some(head) = detail.latest_published() else { return Ok(None) };
    let reference = AssetRevisionRef { asset_id: detail.asset_id, revision: head.revision };
    let manifest = client.resolve_ref(&reference).map_err(redact)?;
    if manifest.kind != AssetKind::Storyboard {
        return Ok(None);
    }
    let file = makepad_asset_client::resolver::select_file(&manifest, FileRole::Source, makepad_asset_client::resolver::TierPreference::PreferWithAnyFallback(makepad_asset_data::DeviceTier::High), 0).map_err(redact)?;
    if file.byte_len > 4 << 20 {
        return Err("the board's source is too large".into());
    }
    let bytes = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(redact)?;
    String::from_utf8(bytes).map(|source| Some((source, reference))).map_err(|_| "the board's source is not UTF-8".into())
}

/// A plain card for the board's library thumbnail: its title on the
/// board's ground (the canvas draws the real thing).
fn board_thumbnail(title: &str) -> Result<PublishThumbnail, String> {
    let (w, h) = (448usize, 256usize);
    let bgra: Vec<u8> = makepad_content_packs::effects::placeholder_bgra(&format!("storyboard {title}"), w, h).iter().flat_map(|pixel| pixel.to_le_bytes()).collect();
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 85).encode(&bgra, w as u16, h as u16, jpeg_encoder::ColorType::Bgra).map_err(|error| format!("thumbnail: {error}"))?;
    Ok(PublishThumbnail::plain(jpeg, ThumbnailMedia::Jpeg, w as u32, h as u32))
}

fn publish_board(client: &mut AssetClient, title: &str, source: &str, dependencies: Vec<AssetRevisionRef>, asset: Option<AssetId>, edit: Option<&str>) -> Result<AssetRevisionRef, String> {
    let files = vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, source.as_bytes().to_vec(), None)];
    let mut bundle = PublishBundle::new(NAMESPACE, AssetKind::Storyboard, if title.is_empty() { "Storyboard".to_string() } else { title.to_string() }, files, board_thumbnail(title)?, PublishRights::generated_cc0());
    bundle.asset_id = asset;
    bundle.dependencies = dependencies;
    bundle.generator = "stage-storyboard".into();
    // Catalog labels are `[a-z0-9][a-z0-9_-]*`: the cut's id as `edit-<id>`.
    bundle.tags = std::iter::once("storyboard".to_string()).chain(edit.map(cut_tag)).collect();
    let published = client.publish_bundle(&bundle).map_err(redact)?;
    Ok(AssetRevisionRef { asset_id: published.asset_id, revision: published.revision })
}

/// A pasted or dropped picture as an image asset: the bytes as they came
/// (PNG or JPEG), a JPEG thumbnail, and where it came from.
fn publish_picture(client: &mut AssetClient, name: &str, bytes: Vec<u8>) -> Result<(AssetRevisionRef, String, bool), String> {
    if bytes.len() > MAX_PICTURE_BYTES {
        return Err(format!("{name} is larger than {} MB", MAX_PICTURE_BYTES >> 20));
    }
    let (image, media) = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        (ImageBuffer::from_png(&bytes).map_err(|error| format!("{name}: not a readable PNG ({error:?})"))?, MediaType::Png)
    } else if bytes.starts_with(&[0xff, 0xd8]) {
        (ImageBuffer::from_jpg(&bytes).map_err(|error| format!("{name}: not a readable JPEG ({error:?})"))?, MediaType::Jpeg)
    } else {
        return Err(format!("{name} is not a PNG or JPEG picture"));
    };
    let (w, h) = (image.width, image.height);
    if w == 0 || h == 0 || image.data.len() != w * h {
        return Err(format!("{name}: an empty picture"));
    }
    // The thumbnail: 256–1024 px on its short side, as the library wants.
    let scale = (512.0 / w.min(h) as f64).clamp(256.0 / w.min(h) as f64, 4096.0 / w.max(h) as f64);
    let (tw, th) = (((w as f64 * scale).round() as usize).max(256), ((h as f64 * scale).round() as usize).max(256));
    let mut rgb = Vec::with_capacity(tw * th * 3);
    for y in 0..th {
        let sy = (y * h / th).min(h - 1);
        for x in 0..tw {
            let pixel = image.data[sy * w + (x * w / tw).min(w - 1)];
            rgb.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]);
        }
    }
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, 85).encode(&rgb, tw as u16, th as u16, jpeg_encoder::ColorType::Rgb).map_err(|error| format!("thumbnail: {error}"))?;
    let thumbnail = PublishThumbnail::plain(jpeg, ThumbnailMedia::Jpeg, tw as u32, th as u32);
    let rights = PublishRights::declared("LicenseRef-Stage-Storyboard-Reference", "Added to a storyboard by the person (pasted or dropped)", name, Redistribution::LanLocal, DerivativePolicy::LocalPreview);
    let title = name.rsplit('/').next().unwrap_or(name).to_string();
    let mut bundle = PublishBundle::new(NAMESPACE, AssetKind::Texture, title.clone(), vec![PublishBundleFile::bytes(FileRole::Texture, media, bytes, Some((w as u32, h as u32)))], thumbnail, rights);
    bundle.generator = "stage-storyboard".into();
    bundle.description = format!("A storyboard reference, {} from {name}", if name.starts_with("pasted") { "pasted" } else { "dropped" });
    bundle.tags = vec!["storyboard".into(), "reference".into()];
    let published = client.publish_bundle(&bundle).map_err(redact)?;
    Ok((AssetRevisionRef { asset_id: published.asset_id, revision: published.revision }, title, h > w))
}

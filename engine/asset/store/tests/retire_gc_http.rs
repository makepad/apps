//! The deletion contract over real sockets: the real `makepad-asset-client`
//! against a real `AssetServer`, publishing, deleting, and collecting.
//!
//! This is the proof the Asset UI's "Delete from store" and "Collect
//! garbage" buttons stand on: after `retire_asset` the alias 404s, search
//! and the listing no longer carry the asset, detail says `retired`, a
//! subscriber is told, and a GC run reclaims exactly the bytes that
//! retirement unlinked — over HTTP, with the same DTOs the UI parses.

use makepad_asset_client::{
    Api, ApiEndpoints, AssetClient, CandidateStateDto, CatalogEventKind, CatalogQuery,
    CatalogSubscriberConfig, CatalogSubscriptionEvent, ClientConfig, ClientError, GcPhaseDto,
    GcRequest, HttpLimits, PublishFile, PublishRequest, PublishThumbnail,
};
use makepad_asset_data::{AssetAlias, AssetKind, FileRole, MediaType, ThumbnailMedia};
use makepad_asset_store::{AssetServer, ServerConfig};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);

fn test_root(name: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("mp_asset_del_{}_{}_{}", std::process::id(), n, name))
}

fn start_server(name: &str) -> (AssetServer, String) {
    let root = test_root(name);
    let mut cfg = ServerConfig::new(root.clone());
    cfg.control_addr = "127.0.0.1:0".parse().unwrap();
    cfg.data_addr = "127.0.0.1:0".parse().unwrap();
    cfg.bootstrap_admin = true;
    cfg.log = false;
    // Tests drive GC explicitly; a background janitor advancing the same run
    // would make the step accounting non-deterministic.
    cfg.gc_janitor_steps = 0;
    cfg.gc_grace_ms = 0;
    let server = AssetServer::start(cfg).expect("server start");
    let token = std::fs::read_to_string(root.join("admin-token"))
        .expect("admin token")
        .trim()
        .to_string();
    (server, token)
}

fn connect(server: &AssetServer, token: &str, cache: &str) -> AssetClient {
    let mut cfg = ClientConfig::new(test_root(cache));
    cfg.token = Some(token.to_string());
    let endpoints = ApiEndpoints { control: server.control_addr(), data: server.data_addr() };
    AssetClient::connect(cfg, endpoints, Some(server.server_id())).expect("connect")
}

fn publish(client: &mut AssetClient, alias: &str, title: &str, fill: u8, len: usize)
    -> makepad_asset_client::Published
{
    publish_rev(client, alias, title, fill, len, None)
}

/// Publish a new REVISION of an existing asset when `asset_id` is given
/// (that is what makes a superseded revision to retire).
fn publish_rev(
    client: &mut AssetClient,
    alias: &str,
    title: &str,
    fill: u8,
    len: usize,
    asset_id: Option<makepad_asset_data::AssetId>,
) -> makepad_asset_client::Published {
    let artifact = vec![fill; len];
    let thumb = vec![fill ^ 0xFF; 1_024];
    let mut request = PublishRequest::new(
        "gen",
        AssetKind::Video,
        title,
        PublishFile {
            bytes: artifact,
            media: MediaType::Mp4,
            role: FileRole::Video,
            media_millis: 1_000,
            dims: None,
        },
        PublishThumbnail {
            bytes: thumb,
            media: ThumbnailMedia::Png,
            width: 512,
            height: 512,
            views: Vec::new(),
        },
    );
    request.alias = Some(AssetAlias::from_str(alias).unwrap());
    request.asset_id = asset_id;
    client.publish_artifact(&request).expect("publish")
}

/// Drive a GC run to completion through the HTTP route, exactly as a UI
/// polling the button would.
fn gc_to_completion(client: &AssetClient, request: GcRequest) -> makepad_asset_client::GcStatusDto {
    let mut status = client.gc_blobs(&request).expect("gc start");
    let deadline = Instant::now() + Duration::from_secs(30);
    while !status.done {
        assert!(Instant::now() < deadline, "gc did not converge: {status:?}");
        status = client.gc_blobs(&request).expect("gc advance");
    }
    status
}

#[test]
fn delete_from_store_then_collect_garbage_over_http() {
    let (mut server, token) = start_server("delete_e2e");
    let mut client = connect(&server, &token, "delete_e2e_cache");

    let mut sub_cfg = CatalogSubscriberConfig::default_v1();
    sub_cfg.wait_ms = 1_000;
    let mut subscriber = client.subscribe_catalog(sub_cfg).expect("subscribe");

    let keep = publish(&mut client, "gen/keeper", "Keeper clip", 0x11, 4_000);
    let drop = publish(&mut client, "gen/doomed", "Doomed clip", 0x22, 6_000);

    // Both are visible: listing, search, alias.
    let page = client.assets_page(Some("gen"), None, 10).expect("listing");
    assert_eq!(page.assets.len(), 2);
    let hits = client
        .catalog_search(&CatalogQuery::browse(10), None)
        .expect("browse");
    assert_eq!(hits.hits.len(), 2);
    let doomed_alias = AssetAlias::from_str("gen/doomed").unwrap();
    assert_eq!(
        client.resolve_alias(&doomed_alias).expect("alias").asset_id,
        drop.asset_id
    );

    // ---- delete ----------------------------------------------------------
    let report = client.retire_asset(&drop.asset_id).expect("retire");
    assert!(!report.already_retired);
    assert_eq!(report.revisions_retired, 1);
    assert_eq!(report.aliases_dropped, 1);
    assert!(report.annotation_cleared);
    // Idempotent over the wire too.
    assert!(client.retire_asset(&drop.asset_id).expect("retire again").already_retired);

    // The alias is gone (indistinguishable from never existing).
    assert!(matches!(
        client.resolve_alias(&doomed_alias),
        Err(ClientError::NotFound { .. })
    ));
    // Search and listing carry only the survivor.
    let hits = client
        .catalog_search(&CatalogQuery::browse(10), None)
        .expect("browse after delete");
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].asset_id, keep.asset_id);
    let page = client.assets_page(Some("gen"), None, 10).expect("listing after delete");
    assert_eq!(page.assets.len(), 1);
    assert_eq!(page.assets[0].asset_id, keep.asset_id);
    // Detail still answers, and says deleted.
    let detail = client.asset_detail(&drop.asset_id).expect("detail after delete");
    assert!(detail.retired);
    assert!(detail.retired_ms.is_some());
    assert_eq!(detail.candidates.len(), 1);
    assert_eq!(detail.candidates[0].state, CandidateStateDto::Retired);
    assert!(detail.candidates[0].retired_ms.is_some());
    assert!(detail.latest_published().is_none());
    // The manifest of a deleted revision reads exactly like an absent one.
    assert!(matches!(
        client.fetch_asset_manifest(&drop.revision),
        Err(ClientError::NotFound { .. })
    ));

    // The subscriber is told, with the retirement kind (this client asks for
    // the vocabulary that has one).
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_retire = false;
    while Instant::now() < deadline && !saw_retire {
        for event in subscriber.poll() {
            if let CatalogSubscriptionEvent::Events { events, .. } = event {
                for ev in events {
                    if ev.asset_id == Some(drop.asset_id)
                        && ev.kind == CatalogEventKind::AssetRetired
                    {
                        assert!(ev.kind.removes_content());
                        saw_retire = true;
                    }
                }
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(saw_retire, "asset_retired must reach the subscriber");
    subscriber.shutdown();

    // ---- collect garbage -------------------------------------------------
    // Dry run first: the exact bytes the real run will free.
    let dry = gc_to_completion(&client, GcRequest::dry_run());
    assert!(dry.dry_run);
    assert_eq!(dry.phase, GcPhaseDto::Done);
    assert_eq!(dry.unreferenced_blobs, 2, "the deleted asset's file + thumbnail");
    assert_eq!(dry.unreferenced_bytes, 6_000 + 1_024);
    assert_eq!(dry.deleted_blobs, 0);
    // Nothing was deleted by the preview.
    assert!(client.blob_head(&drop.artifact_blob).is_ok());

    let real = gc_to_completion(&client, GcRequest::collect());
    assert!(!real.dry_run);
    assert_eq!(real.deleted_blobs, dry.unreferenced_blobs);
    assert_eq!(real.deleted_bytes, dry.unreferenced_bytes);

    // The deleted asset's bytes are unreachable; the survivor's are intact.
    assert!(matches!(
        client.blob_head(&drop.artifact_blob),
        Err(ClientError::NotFound { .. })
    ));
    assert_eq!(
        client
            .fetch_blob_bytes(&keep.artifact_blob, Some(4_000))
            .expect("survivor bytes")
            .len(),
        4_000
    );

    // Status without starting a run reports the finished one; a further
    // collection finds nothing.
    let status = client.gc_status().expect("gc status");
    assert!(status.done);
    assert_eq!(status.deleted_blobs, 2);
    let again = gc_to_completion(&client, GcRequest::collect());
    assert_eq!(again.deleted_blobs, 0);
    assert!(!client.gc_cancel().expect("cancel idle"));

    server.shutdown();
}

#[test]
fn retiring_a_superseded_revision_keeps_the_head_serving() {
    let (mut server, token) = start_server("retire_revision_e2e");
    let mut client = connect(&server, &token, "retire_revision_cache");

    let v1 = publish(&mut client, "gen/clip", "Clip v1", 0x33, 3_000);
    let v2 = publish_rev(&mut client, "gen/clip", "Clip v2", 0x44, 3_500, Some(v1.asset_id));
    assert_eq!(v1.asset_id, v2.asset_id);
    assert_ne!(v1.revision, v2.revision);
    let alias = AssetAlias::from_str("gen/clip").unwrap();
    assert_eq!(client.resolve_alias(&alias).expect("alias").head_revision, v2.revision);

    let report = client.retire_revision(&v1.asset_id, &v1.revision).expect("retire v1");
    assert!(!report.already_retired);
    assert!(client
        .retire_revision(&v1.asset_id, &v1.revision)
        .expect("retire v1 again")
        .already_retired);

    // The head still serves and the asset is still listed.
    assert_eq!(client.resolve_alias(&alias).expect("alias").head_revision, v2.revision);
    let page = client.assets_page(Some("gen"), None, 10).expect("listing");
    assert_eq!(page.assets.len(), 1);
    let detail = client.asset_detail(&v1.asset_id).expect("detail");
    assert!(!detail.retired);
    let states: Vec<_> = detail
        .candidates
        .iter()
        .map(|c| (c.revision, c.state))
        .collect();
    assert!(states.contains(&(v1.revision, CandidateStateDto::Retired)));
    assert!(states.contains(&(v2.revision, CandidateStateDto::Published)));

    // Collection frees the superseded revision's bytes only.
    let status = gc_to_completion(&client, GcRequest::collect());
    assert_eq!(status.deleted_blobs, 2, "v1's file + thumbnail");
    assert!(matches!(
        client.blob_head(&v1.artifact_blob),
        Err(ClientError::NotFound { .. })
    ));
    assert!(client.blob_head(&v2.artifact_blob).is_ok());

    server.shutdown();
}

#[test]
fn retention_over_http_trims_history_without_touching_heads() {
    let (mut server, token) = start_server("retention_e2e");
    let mut client = connect(&server, &token, "retention_cache");
    let mut published = Vec::new();
    for i in 0..3u8 {
        let asset_id = published.last().map(|p: &makepad_asset_client::Published| p.asset_id);
        published.push(publish_rev(
            &mut client,
            "gen/series",
            &format!("Series v{i}"),
            0x50 + i,
            2_000 + i as usize,
            asset_id,
        ));
    }
    let request = GcRequest { retain_per_asset: Some(1), ..GcRequest::default() };
    let status = gc_to_completion(&client, request);
    // Two superseded revisions retired; the head survives.
    assert_eq!(status.retired_revisions, 2);
    assert_eq!(status.deleted_blobs, 4);
    let head = published.last().unwrap();
    assert!(client.blob_head(&head.artifact_blob).is_ok());
    for old in &published[..2] {
        assert!(matches!(
            client.blob_head(&old.artifact_blob),
            Err(ClientError::NotFound { .. })
        ));
    }
    let alias = AssetAlias::from_str("gen/series").unwrap();
    assert_eq!(client.resolve_alias(&alias).expect("alias").head_revision, head.revision);

    // Retention retires revisions inside the core, without any route being
    // called: detail must still report the real lifecycle (it reads the
    // catalog, not a mirror).
    let detail = client.asset_detail(&head.asset_id).expect("detail");
    assert!(!detail.retired);
    let retired: Vec<_> = detail
        .candidates
        .iter()
        .filter(|c| c.state == CandidateStateDto::Retired)
        .map(|c| c.revision)
        .collect();
    assert_eq!(retired.len(), 2);
    assert!(!retired.contains(&head.revision));
    assert_eq!(
        detail.latest_published().expect("head candidate").revision,
        head.revision
    );

    server.shutdown();
}

#[test]
fn deletion_and_collection_require_credentials() {
    let (mut server, token) = start_server("delete_auth");
    let mut client = connect(&server, &token, "delete_auth_cache");
    let published = publish(&mut client, "gen/guarded", "Guarded", 0x66, 2_000);

    // A well-shaped but unknown token is refused uniformly on every one of
    // the new routes — no oracle, and nothing is deleted.
    let stranger = Api::new(
        ApiEndpoints { control: server.control_addr(), data: server.data_addr() },
        HttpLimits::default_v1(),
        Some(format!("mpat_{}", "ab".repeat(32))),
    )
    .expect("api");
    assert!(matches!(
        stranger.retire_asset(&published.asset_id),
        Err(ClientError::Unauthenticated)
    ));
    assert!(matches!(
        stranger.retire_revision(&published.asset_id, &published.revision),
        Err(ClientError::Unauthenticated)
    ));
    assert!(matches!(
        stranger.gc_blobs(&GcRequest::dry_run()),
        Err(ClientError::Unauthenticated)
    ));
    assert!(matches!(stranger.gc_status(), Err(ClientError::Unauthenticated)));
    assert!(matches!(stranger.gc_cancel(), Err(ClientError::Unauthenticated)));

    // The asset is untouched.
    let detail = client.asset_detail(&published.asset_id).expect("detail");
    assert!(!detail.retired);

    server.shutdown();
}

fn render_bundle_of(title: &str, fill: u8) -> makepad_asset_client::PublishBundle {
    use makepad_asset_client::{PublishBundle, PublishBundleFile, PublishRights};
    let mut bundle = PublishBundle::new(
        "elsewhere",
        AssetKind::Video,
        title,
        vec![PublishBundleFile::bytes(FileRole::Video, MediaType::Mp4, vec![fill; 7_000], None)],
        PublishThumbnail {
            bytes: vec![fill ^ 0xFF; 1_024],
            media: ThumbnailMedia::Png,
            width: 512,
            height: 512,
            views: Vec::new(),
        },
        PublishRights::generated_cc0(),
    );
    bundle.media_millis = 1_000;
    bundle
}

/// Renders and the REMOVE guard over the wire: a render is a `Video` in its
/// source's namespace whose one dependency is the source, found through
/// `<ns>/render/<source id>`; `dependents` names it before the source is
/// retired; the retired source stays fetchable (manifest and bytes) for as
/// long as the render pins it, and not a moment longer.
#[test]
fn renders_pin_their_source_and_dependents_answer_who_would_break() {
    use makepad_asset_client::render_alias;
    use makepad_asset_data::AssetRevisionRef;
    let (mut server, token) = start_server("render_e2e");
    let mut client = connect(&server, &token, "render_e2e_cache");

    let clip = publish(&mut client, "gen/source-clip", "Source clip", 0x31, 5_000);
    let source = AssetRevisionRef { asset_id: clip.asset_id, revision: clip.revision };
    assert_eq!(client.render_of(&clip.asset_id).expect("no render yet"), None);
    let none = client.dependents(&clip.asset_id, None).expect("dependents");
    assert_eq!((none.total, none.dependents.len(), none.cursor), (0, 0, None));

    let render = client
        .publish_render(source, render_bundle_of("Source clip render", 0x41))
        .expect("publish render");
    let alias = render_alias("gen", &clip.asset_id);
    assert!(alias.starts_with("gen/render/"));
    assert_eq!(render.alias.as_ref().map(|a| a.as_str().to_string()), Some(alias.clone()));
    let render_ref = AssetRevisionRef { asset_id: render.asset_id, revision: render.revision };
    assert_eq!(client.render_of(&clip.asset_id).expect("render_of"), Some((render_ref, source)));
    assert_eq!(client.asset_detail(&render.asset_id).expect("detail").namespace, "gen");

    let used = client.dependents(&clip.asset_id, None).expect("dependents");
    assert_eq!((used.total, used.hidden), (1, 0));
    assert_eq!(used.dependents[0].asset_id, render.asset_id);
    assert_eq!(used.dependents[0].revision, render.revision);
    assert_eq!(used.dependents[0].kind, AssetKind::Video);
    assert_eq!(used.dependents[0].title, "Source clip render");

    // Retiring the source anyway: the render still pins it, so its manifest
    // and bytes stay fetchable by a client that never cached them.
    client.retire_asset(&clip.asset_id).expect("retire source");
    assert_eq!(gc_to_completion(&client, GcRequest::collect()).deleted_blobs, 0);
    let mut fresh = connect(&server, &token, "render_e2e_fresh_cache");
    let manifest = fresh.fetch_asset_manifest(&clip.revision).expect("pinned retired manifest");
    assert_eq!(
        fresh.fetch_blob_bytes(&manifest.files[0].blob, Some(5_000)).expect("pinned retired bytes"),
        vec![0x31; 5_000]
    );
    // A new render of the retired source is refused: retirement is terminal.
    assert!(matches!(
        client.publish_render(source, render_bundle_of("Too late", 0x51)),
        Err(ClientError::InvalidInput { what: "render source retired" })
    ));

    // With the render gone, the source is unreachable and all of it is garbage.
    client.retire_asset(&render.asset_id).expect("retire render");
    assert!(client.dependents(&clip.asset_id, None).expect("dependents").dependents.is_empty());
    let mut late = connect(&server, &token, "render_e2e_late_cache");
    assert!(matches!(late.fetch_asset_manifest(&clip.revision), Err(ClientError::NotFound { .. })));
    assert_eq!(gc_to_completion(&client, GcRequest::collect()).deleted_blobs, 4);
    server.shutdown();
}

/// Renders of one source finishing out of order: a late render of an older
/// source revision is published but does not take the alias back; and a
/// render alias pointing at something that is not a render of the source
/// is refused by `render_of`, not believed.
#[test]
fn a_late_render_of_an_older_source_leaves_the_alias_alone() {
    use makepad_asset_data::AssetRevisionRef;
    let (mut server, token) = start_server("render_order");
    let mut client = connect(&server, &token, "render_order_cache");
    let v1 = publish(&mut client, "gen/edit-src", "Edit source", 0x61, 3_000);
    let v2 = publish_rev(&mut client, "gen/edit-src", "Edit source", 0x62, 3_000, Some(v1.asset_id));
    let src1 = AssetRevisionRef { asset_id: v1.asset_id, revision: v1.revision };
    let src2 = AssetRevisionRef { asset_id: v2.asset_id, revision: v2.revision };

    let new = client.publish_render(src2, render_bundle_of("Render v2", 0x71)).expect("render v2");
    let late = client.publish_render(src1, render_bundle_of("Render v1", 0x72)).expect("render v1");
    assert!(new.alias.is_some());
    assert_eq!(late.alias, None);
    let head = client.render_of(&v1.asset_id).expect("render_of").expect("a render");
    assert_eq!(head, (AssetRevisionRef { asset_id: new.asset_id, revision: new.revision }, src2));
    // Re-rendering the newest revision moves the alias as usual.
    let again = client.publish_render(src2, render_bundle_of("Render v2 again", 0x73)).expect("again");
    assert!(again.alias.is_some());

    // Someone points the render alias at a plain clip: refused.
    let stranger = publish(&mut client, "gen/unrelated", "Unrelated", 0x74, 2_000);
    let alias = AssetAlias::from_str(&makepad_asset_client::render_alias("gen", &v1.asset_id)).unwrap();
    let api = Api::new(
        ApiEndpoints { control: server.control_addr(), data: server.data_addr() },
        HttpLimits::default_v1(),
        Some(token.clone()),
    )
    .expect("api");
    api.put_alias(&alias, &stranger.asset_id, &stranger.revision).expect("repoint");
    assert!(matches!(
        client.render_of(&v1.asset_id),
        Err(ClientError::Protocol { what: "render alias target" })
    ));
    server.shutdown();
}

/// A bad catalog label (here a `:` tag) is refused by the client before
/// anything reaches the store: no asset registered, no blob uploaded.
#[test]
fn a_bad_tag_fails_locally_before_any_upload() {
    let (mut server, token) = start_server("bad_tag");
    let mut client = connect(&server, &token, "bad_tag_cache");
    let artifact = vec![0x5A; 3_000];
    let mut request = PublishRequest::new(
        "gen",
        AssetKind::Video,
        "Tagged clip",
        PublishFile {
            bytes: artifact.clone(),
            media: MediaType::Mp4,
            role: FileRole::Video,
            media_millis: 1_000,
            dims: None,
        },
        PublishThumbnail {
            bytes: vec![0xA5; 1_024],
            media: ThumbnailMedia::Png,
            width: 512,
            height: 512,
            views: Vec::new(),
        },
    );
    request.tags = vec!["music".into(), "flow:preset".into()];
    assert_eq!(
        client.publish_artifact(&request).unwrap_err(),
        ClientError::InvalidInput { what: "annotation label charset" }
    );
    assert!(client.assets_page(Some("gen"), None, 10).expect("listing").assets.is_empty());
    assert!(matches!(
        client.blob_head(&makepad_asset_data::BlobId::hash_of(&artifact)),
        Err(ClientError::NotFound { .. })
    ));
    request.tags = vec!["music".into(), "flow-preset".into()];
    client.publish_artifact(&request).expect("a valid tag publishes");
    server.shutdown();
}

#[test]
fn repeated_image_files_survive_gc_until_their_last_live_asset_retires() {
    use makepad_asset_client::PublishBundleFile;
    use makepad_asset_data::BlobId;

    let (mut server, token) = start_server("image_gc");
    let mut client = connect(&server, &token, "image_gc_cache");
    let images = [vec![0x61], vec![0x62], vec![0x63]];
    let blobs: Vec<_> = images.iter().map(|bytes| BlobId::hash_of(bytes)).collect();
    let mut first = render_bundle_of("Three images", 0x31);
    for bytes in &images {
        first.files.push(PublishBundleFile::bytes(FileRole::Image, MediaType::Bin, bytes.clone(), None));
    }
    let published = client.publish_bundle(&first).expect("three images publish in one slot");
    let mut second = render_bundle_of("Shared image", 0x32);
    second.files.push(PublishBundleFile::bytes(FileRole::Image, MediaType::Bin, images[0].clone(), None));
    client.publish_bundle(&second).expect("a second asset may share an image");

    gc_to_completion(&client, GcRequest::collect());
    for blob in &blobs {
        assert!(client.blob_head(blob).is_ok(), "GC must retain every live image: {blob}");
    }
    client.retire_asset(&published.asset_id).expect("retire the first image owner");
    gc_to_completion(&client, GcRequest::collect());
    for blob in &blobs[1..] {
        assert!(matches!(client.blob_head(blob), Err(ClientError::NotFound { .. })),
            "GC must remove each unshared image after retirement: {blob}");
    }
    assert!(client.blob_head(&blobs[0]).is_ok(), "the second live asset keeps its shared image");
    server.shutdown();
}

// ---- streamed file slots ---------------------------------------------------

/// Deterministic, non-repeating-per-seed content of `len` bytes.
fn render_bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| ((i as u64 * 2_654_435_761 + seed as u64 * 97) >> 7) as u8).collect()
}

fn file_render_bundle(path: &std::path::Path) -> makepad_asset_client::PublishBundle {
    use makepad_asset_client::{PublishBundle, PublishBundleFile, PublishRights};
    let mut bundle = PublishBundle::new(
        "gen",
        AssetKind::Video,
        "Long render",
        vec![
            PublishBundleFile::file(FileRole::Video, MediaType::Mp4, path.to_path_buf(), None),
            PublishBundleFile::bytes(
                FileRole::CaptionsSrt,
                MediaType::Text,
                b"1\n00:00:00,000 --> 00:00:01,000\nHello\n".to_vec(),
                None,
            ),
            PublishBundleFile::bytes(
                FileRole::CaptionsVtt,
                MediaType::Text,
                b"WEBVTT\n\n00:00.000 --> 00:01.000\nHello\n".to_vec(),
                None,
            ),
        ],
        PublishThumbnail {
            bytes: vec![0x3C; 1_024],
            media: ThumbnailMedia::Png,
            width: 512,
            height: 512,
            views: Vec::new(),
        },
        PublishRights::generated_cc0(),
    );
    bundle.media_millis = 4_000;
    bundle
}

/// A file slot is hashed and streamed in 1 MiB pieces with progress, lands
/// byte-exact next to its caption tracks, and publishes through
/// `publish_render` unchanged; a file that changes after hashing (same
/// length or not) and an abort mid-upload refuse without committing a blob.
#[test]
fn a_render_publishes_from_a_file_streamed_in_pieces() {
    use makepad_asset_client::PublishStage;
    use makepad_asset_data::{AssetRevisionRef, BlobId};
    let (mut server, token) = start_server("stream_file");
    let mut client = connect(&server, &token, "stream_file_cache");
    let dir = test_root("stream_file_src");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("render.mp4");
    let len = 3 * 1024 * 1024 + 7;
    let content = render_bytes(len, 1);
    std::fs::write(&path, &content).unwrap();

    let mut stages: Vec<PublishStage> = Vec::new();
    let mut record = |stage: &PublishStage| stages.push(stage.clone());
    let published = client
        .publish_bundle_with(&file_render_bundle(&path), Some(&mut record), &|| false)
        .expect("streamed publish");
    let hashing: Vec<_> = stages
        .iter()
        .filter_map(|s| match s {
            PublishStage::HashingFile { done, total, .. } => Some((*done, *total)),
            _ => None,
        })
        .collect();
    let sent: Vec<_> = stages
        .iter()
        .filter_map(|s| match s {
            PublishStage::UploadProgress { sent, total, .. } => Some((*sent, *total)),
            _ => None,
        })
        .collect();
    assert_eq!((hashing.len(), hashing.last().copied()), (4, Some((len as u64, len as u64))));
    assert_eq!((sent.len(), sent.last().copied()), (4, Some((len as u64, len as u64))));

    let manifest = client.fetch_asset_manifest(&published.revision).expect("manifest");
    let video = manifest.files.iter().find(|f| f.role == FileRole::Video).unwrap();
    assert_eq!((video.blob, video.byte_len), (BlobId::hash_of(&content), len as u64));
    assert_eq!(client.fetch_blob_bytes(&video.blob, Some(len as u64)).unwrap(), content);
    let captions: Vec<_> = manifest.files.iter().filter(|f| f.role.is_captions()).map(|f| (f.role, f.lod)).collect();
    assert_eq!(captions, vec![(FileRole::CaptionsSrt, 0), (FileRole::CaptionsVtt, 0)]);

    // The same file slot works through publish_render unchanged.
    let source = AssetRevisionRef { asset_id: published.asset_id, revision: published.revision };
    let other = dir.join("render2.mp4");
    std::fs::write(&other, render_bytes(len, 2)).unwrap();
    client.publish_render(source, file_render_bundle(&other)).expect("render from a file");

    // Content changes after hashing, same length: the store refuses the body.
    let changed = render_bytes(len, 3);
    std::fs::write(&path, render_bytes(len, 4)).unwrap();
    let swap_path = path.clone();
    let swapped = changed.clone();
    let mut swap = move |stage: &PublishStage| {
        if matches!(stage, PublishStage::RegisteringAsset) {
            std::fs::write(&swap_path, &swapped).unwrap();
        }
    };
    let refused = client.publish_bundle_with(&file_render_bundle(&path), Some(&mut swap), &|| false);
    assert!(matches!(refused, Err(ClientError::Server { status: 400..=499, .. })), "{refused:?}");
    for bytes in [&changed, &render_bytes(len, 4)] {
        assert!(matches!(client.blob_head(&BlobId::hash_of(bytes)), Err(ClientError::NotFound { .. })));
    }

    // Length changes after hashing (here: once hashing is done, at
    // registration): refused locally before the upload. A change DURING
    // hashing is caught there ("publish file changed while hashing").
    std::fs::write(&path, render_bytes(len, 5)).unwrap();
    let grow_path = path.clone();
    let mut grow = move |stage: &PublishStage| {
        if matches!(stage, PublishStage::RegisteringAsset) {
            std::fs::write(&grow_path, render_bytes(len + 1, 5)).unwrap();
        }
    };
    let grown = client.publish_bundle_with(&file_render_bundle(&path), Some(&mut grow), &|| false);
    assert!(
        matches!(grown, Err(ClientError::SizeMismatch { what: "publish file changed since hashing", .. })),
        "{grown:?}"
    );

    // Abort mid-upload: cancelled, and no partial blob is committed.
    let aborting = render_bytes(len, 6);
    std::fs::write(&path, &aborting).unwrap();
    let uploaded = std::cell::Cell::new(false);
    let mut watch = |stage: &PublishStage| {
        if matches!(stage, PublishStage::UploadProgress { .. }) {
            uploaded.set(true);
        }
    };
    assert_eq!(
        client
            .publish_bundle_with(&file_render_bundle(&path), Some(&mut watch), &|| uploaded.get())
            .unwrap_err(),
        ClientError::Cancelled
    );
    assert!(matches!(client.blob_head(&BlobId::hash_of(&aborting)), Err(ClientError::NotFound { .. })));
    server.shutdown();
}

/// Undo a delete over HTTP: after `restore_asset` the asset is back in the
/// listing and search with its title, its alias resolves, detail says
/// published, the manifest serves again, and a subscriber sees it published.
#[test]
fn restore_after_delete_over_http() {
    let (_server, token) = start_server("restore_e2e");
    let mut client = connect(&_server, &token, "restore_e2e_cache");
    let back = publish(&mut client, "gen/comeback", "Comeback clip", 0x33, 3_000);
    client.retire_asset(&back.asset_id).expect("retire");

    let mut sub_cfg = CatalogSubscriberConfig::default_v1();
    sub_cfg.wait_ms = 1_000;
    let mut subscriber = client.subscribe_catalog(sub_cfg).expect("subscribe");
    // The feed starts at the journal's tail when it first connects: wait for
    // that, so the restore's events land after the cursor.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut ready = false;
    while !ready {
        assert!(Instant::now() < deadline, "subscriber never became ready");
        ready = subscriber
            .poll()
            .iter()
            .any(|event| matches!(event, CatalogSubscriptionEvent::Ready { .. }));
        std::thread::sleep(Duration::from_millis(10));
    }

    let report = client.restore_asset(&back.asset_id).expect("restore");
    assert!(!report.already_live);
    assert_eq!(report.restored.len(), 1);
    assert_eq!(report.restored[0].asset_id, back.asset_id);
    assert_eq!(report.restored[0].revisions, vec![back.revision]);
    assert_eq!(report.restored[0].aliases_restored, 1);
    assert!(report.restored[0].annotation_restored);
    assert!(client.restore_asset(&back.asset_id).expect("restore again").already_live);

    let page = client.assets_page(Some("gen"), None, 10).expect("listing");
    assert_eq!(page.assets.len(), 1);
    let hits = client.catalog_search(&CatalogQuery::browse(10), None).expect("browse");
    assert_eq!(hits.hits.len(), 1);
    assert_eq!(hits.hits[0].title, "Comeback clip");
    let alias = AssetAlias::from_str("gen/comeback").unwrap();
    assert_eq!(client.resolve_alias(&alias).expect("alias").asset_id, back.asset_id);
    let detail = client.asset_detail(&back.asset_id).expect("detail");
    assert!(!detail.retired);
    assert_eq!(detail.candidates[0].state, CandidateStateDto::Published);
    assert!(client.fetch_asset_manifest(&back.revision).is_ok());

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut saw_publish = false;
    while Instant::now() < deadline && !saw_publish {
        for event in subscriber.poll() {
            if let CatalogSubscriptionEvent::Events { events, .. } = event {
                saw_publish |= events.iter().any(|ev| {
                    ev.asset_id == Some(back.asset_id) && ev.kind == CatalogEventKind::AssetPublished
                });
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(saw_publish, "a restored asset reaches subscribers as published");
    subscriber.shutdown();
}

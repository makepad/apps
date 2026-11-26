//! `/v1/events` end-to-end: publish notification, kind filtering, long-poll
//! wakeup/timeout, cursor resume without duplicates, retention gaps, auth,
//! shutdown behavior, and slow/parked clients — all over real sockets
//! against a real server instance.

mod common;

use common::*;
use makepad_asset_client::http::BytesBody;
use makepad_asset_store::json::Value;
use makepad_asset_data::AssetId;
use std::time::{Duration, Instant};

fn events_of(resp: &Response) -> Vec<Value> {
    resp.json().get("events").and_then(Value::as_arr).map(<[Value]>::to_vec).expect("events array")
}

fn cursor_of(resp: &Response) -> String {
    resp.str_field("cursor")
}

fn gap_of(resp: &Response) -> bool {
    resp.json().get("gap").and_then(Value::as_bool).expect("gap flag")
}

fn ev_str<'v>(ev: &'v Value, key: &str) -> Option<&'v str> {
    ev.get(key).and_then(Value::as_str)
}

/// Mint a principal allowed to upload/register/publish/alias in `ns`, and
/// hand back control+data clients bearing its token.
fn content_clients(ts: &TestServer, admin: &str, ns: &str) -> (Client, Client) {
    let mut admin_control = ts.control(Some(admin));
    let token = principal_with(
        &mut admin_control,
        &[
            ("blob_write", ns),
            ("asset_register", ns),
            ("asset_publish", ns),
            ("preview_publish", ns),
            ("alias_write", ns),
        ],
    );
    (ts.control(Some(&token)), ts.data(Some(&token)))
}

fn preview_api(ts: &TestServer) -> makepad_asset_client::Api {
    use makepad_asset_client::{Api, ApiEndpoints, HttpLimits};
    Api::new(ApiEndpoints { control: ts.server.control_addr(), data: ts.server.data_addr() },
        HttpLimits::default_v1(), Some(ts.admin_token())).unwrap()
}

fn preview_catalog_counts(root: &std::path::Path) -> [i64; 3] {
    let mut db = makepad_sqlite::Database::open(&root.join("catalog.sqlite3")).unwrap();
    ["blobs", "asset_revisions", "asset_aliases"].map(|table| {
        let result = db.query(&format!("SELECT COUNT(*) FROM {table}"), &[]).unwrap();
        result.rows[0][0].as_integer().unwrap()
    })
}

fn assert_preview_spool_empty(root: &std::path::Path) {
    assert_eq!(std::fs::read_dir(root.join("previews")).unwrap().count(), 0);
}

#[test]
fn draft_admission_refuses_before_body_and_supersedes_swaps_once() {
    use makepad_asset_client::{Api, ApiEndpoints, ClientError, HttpLimits};
    use makepad_asset_client::http::{http_call_pooled, ConnPool, Method, Request, StreamBody};
    use makepad_asset_data::AssetAlias;
    use makepad_asset_store::PreviewBudget;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountedBody { len: u64, read: AtomicUsize }
    impl StreamBody for CountedBody {
        fn len(&self) -> u64 { self.len }
        fn read_at(&self, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = (self.len - offset).min(buf.len() as u64) as usize;
            self.read.fetch_add(n, Ordering::Relaxed); buf[..n].fill(1); Ok(n)
        }
    }
    let mut ts = start_server_with("preview_books", |cfg| cfg.preview_disk = PreviewBudget::Bytes(100));
    let api = preview_api(&ts);
    let admin = ts.admin_token();
    let mut control = ts.control(Some(&admin));
    let peer_token = principal_with(&mut control, &[("preview_publish", "gen")]);
    let peer = Api::new(ApiEndpoints { control: ts.server.control_addr(), data: ts.server.data_addr() }, HttpLimits::default_v1(), Some(peer_token)).unwrap();
    api.open_model_preview(&AssetAlias::new("gen/drafts/upload").unwrap(), "upload", "cage", None).unwrap();
    peer.open_model_preview(&AssetAlias::new("gen/drafts/peer").unwrap(), "peer", "cage", None).unwrap();
    peer.upload_model_preview_part("peer", "part", &[], &BytesBody::new(&[1; 50], &|| false)).unwrap();
    let pool = ConnPool::default();
    for (len, status) in [(60, 503), (101, 507)] {
        let body = CountedBody { len, read: AtomicUsize::new(0) };
        let mut req = Request::get("/v1/model-preview-sessions/upload/parts/part");
        req.method = Method::Put; req.bearer = Some(&admin); req.body_stream = Some(&body); req.expect_continue = true;
        let response = http_call_pooled(ts.server.data_addr(), &req, &HttpLimits::default_v1(), Some(&pool)).unwrap();
        assert_eq!(response.head().status, status, "admission refusal returns its specified status");
        let value = makepad_asset_store::json::parse(&response.read_full(2048).unwrap()).unwrap();
        assert_eq!(value.get("len").and_then(Value::as_u64), Some(len), "refusal carries the declared length");
        assert_eq!(value.get("budget").and_then(Value::as_u64), Some(100), "refusal carries the store budget");
        if status == 503 {
            assert_eq!(value.get("used").and_then(Value::as_u64), Some(50), "waiting response carries the current book total");
            assert_eq!(value.get("share").and_then(Value::as_u64), Some(50), "waiting response carries this author's share");
        }
        assert_eq!(body.read.load(Ordering::Relaxed), 0, "a refusal never reads the streamed body source");
        assert_eq!(pool.idle_len(), 0, "a refused request closes its connection");
        let mut health = Request::get("/v1/health"); health.bearer = Some(&admin);
        let response = http_call_pooled(ts.server.data_addr(), &health, &HttpLimits::default_v1(), Some(&pool)).unwrap();
        assert_eq!(response.head().status, 200, "the next request works on a new connection");
        response.read_full(4096).unwrap();
        assert!(matches!(api.upload_model_preview_part("upload", "part", &[], &body), Err(ClientError::Server { status: found, .. }) if found == status), "Api preserves 503 and 507 as server errors");
        assert_eq!(body.read.load(Ordering::Relaxed), 0, "Api also refuses before consuming the source");
    }
    api.upload_model_preview_part("upload", "old", &[], &BytesBody::new(&[2; 10], &|| false)).unwrap();
    let cursor = api.events_page(None, 0, 128, None, &[]).unwrap().cursor;
    api.upload_model_preview_part("upload", "new", &["old"], &BytesBody::new(&[3; 10], &|| false)).unwrap();
    let page = api.events_page(Some(&cursor), 0, 128, None, &[]).unwrap();
    assert_eq!(page.events.len(), 1, "supersedes emits one atomic delta over HTTP");
    let delta = page.events[0].model_preview.as_ref().unwrap();
    assert_eq!(delta.parts[0].name, "new", "the swap carries the incoming part");
    assert_eq!(delta.removed, ["old"], "the swap removes the old part in that same event");
    drop((api, peer, control, pool));
    ts.server.shutdown(); let root = ts.root.clone(); drop(ts); std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn draft_preview_http_bootstraps_all_cages_and_streams_u64_parts_without_catalog_writes() {
    use makepad_asset_data::{AssetAlias, BlobId};
    let mut ts = start_server("preview_bootstrap");
    let before = preview_catalog_counts(&ts.root);
    let api = preview_api(&ts);
    let alias = AssetAlias::new("gen/drafts/car-0").unwrap();
    assert!(api.resolve_alias(&alias).is_err());
    for id in 0..40 {
        let alias = AssetAlias::new(format!("gen/drafts/car-{id}")).unwrap();
        api.open_model_preview(&alias, &format!("edit-{id}"), "{\"kind\":\"editable_creation\"}", None).unwrap();
    }
    // limit applies to deltas, never truncates an atomic late-join snapshot.
    let page = api.events_page(None, 0, 1, None, &[]).unwrap();
    assert_eq!(page.events.len(), 40, "session admission has no count cap");
    assert!(page.events.iter().all(|e| e.model_preview.as_ref().is_some_and(|p| p.open && p.parts.is_empty())));
    let cursor = page.cursor;
    api.update_model_preview("edit-0", None, &[], &[]).unwrap();
    let unchanged = api.events_page(Some(&cursor), 0, 1, None, &[]).unwrap();
    assert_eq!(unchanged.cursor, cursor);
    assert!(unchanged.events.is_empty());
    let name = format!("draft-{}", u64::MAX);
    let mesh = b"ephemeral draft GLB bytes";
    let token = api.upload_model_preview_part("edit-0", &name, &[], &BytesBody::new(mesh, &|| false)).unwrap();
    let delta = api.events_page(Some(&cursor), 0, 1, None, &[]).unwrap();
    assert_eq!(delta.events.len(), 1);
    let preview = delta.events[0].model_preview.as_ref().unwrap();
    assert!(!preview.open);
    assert_eq!(preview.parts[0].name, name);
    assert_eq!(preview.parts[0].bytes, mesh.len() as u64);
    let fetched = api.fetch_model_preview_mesh(&token, None).unwrap();
    assert_eq!(fetched, mesh);
    assert_eq!(token, format!("pmesh_{}", makepad_asset_data::sha256_hex(&fetched)));
    assert_eq!(preview_catalog_counts(&ts.root), before, "preview must create no catalog rows");
    assert!(api.resolve_alias(&AssetAlias::new("gen/drafts/car-0").unwrap()).is_err());
    assert!(api.blob_head(&BlobId::hash_of(mesh)).is_err(), "preview mesh must not enter CAS");
    api.clear_model_preview("edit-0").unwrap();
    assert_preview_spool_empty(&ts.root);
    assert!(api.fetch_model_preview_mesh(&token, None).is_err());
    let snapshot = api.events_page(None, 0, 1, None, &[]).unwrap();
    assert_eq!(snapshot.events.len(), 39);
    assert!(!snapshot.events.iter().any(|e| e.model_preview.as_ref().unwrap().session == "edit-0"));
    // Part names still follow the wire grammar and the alias namespace is
    // not opened to unrelated generated or published models.
    assert!(api.upload_model_preview_part("edit-1", &"x".repeat(33), &[], &BytesBody::new(mesh, &|| false)).is_err());
    assert!(api.open_model_preview(&AssetAlias::new("gen/models/not-a-draft").unwrap(), "wrong", "", None).is_err());
    let mut raw = ts.control(Some(&ts.admin_token()));
    let denied = raw.post_json("/v1/model-previews", &jobj(vec![
        ("op", jstr("open")), ("session", jstr("wrong")),
        ("alias", jstr("gen/other/wrong")), ("program", jstr("")),
    ]));
    assert_eq!(denied.status, 400);
    // The books also replace the old decoder and metadata-delta count checks.
    let names: Vec<_> = (0..40).map(|n| format!("part-{n}")).collect();
    for name in &names { api.upload_model_preview_part("edit-1", name, &[], &BytesBody::new(b"x", &|| false)).unwrap(); }
    let snapshot = api.events_page(None, 0, 1, None, &[]).unwrap();
    let preview = snapshot.events.iter().filter_map(|event| event.model_preview.as_ref()).find(|p| p.session == "edit-1").unwrap();
    assert_eq!(preview.parts.len(), 40, "a complete snapshot decodes more than 32 parts");
    let renamed: Vec<_> = names.iter().enumerate().map(|(n, name)| makepad_asset_client::ModelPreviewRenameDto { from: name.clone(), to: format!("renamed-{n}") }).collect();
    let program = "x".repeat(12_001);
    api.update_model_preview("edit-1", Some(&program), &[], &renamed).unwrap();
    let delta = api.events_page(Some(&snapshot.cursor), 0, 128, None, &[]).unwrap();
    let preview = delta.events[0].model_preview.as_ref().unwrap();
    assert_eq!(preview.renamed.len(), 40, "metadata deltas accept and decode every rename");
    assert_eq!(preview.program.as_deref(), Some(program.as_str()), "program size is admitted by the metadata book");
    let supersedes: Vec<_> = renamed.iter().map(|rename| rename.to.as_str()).collect();
    api.upload_model_preview_part("edit-1", "replacement", &supersedes, &BytesBody::new(b"y", &|| false)).unwrap();
    let delta = api.events_page(Some(&delta.cursor), 0, 128, None, &[]).unwrap();
    assert_eq!(delta.events.len(), 1, "a large supersedes list still swaps atomically");
    assert_eq!(delta.events[0].model_preview.as_ref().unwrap().removed.len(), 40, "receivers decode every superseded removal");
    api.clear_model_preview("edit-1").unwrap();
    drop(raw);
    drop(api);
    ts.server.shutdown();
    let root = ts.root.clone();
    drop(ts);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn preview_file_ranges_inflight_replacement_and_restart() {
    use makepad_asset_client::http::{http_call, HttpLimits, Request};
    use makepad_asset_data::AssetAlias;
    let mut ts = start_server_with("preview_spool", |cfg| cfg.budgets.max_blob_bytes = 4);
    let api = preview_api(&ts);
    let admin = ts.admin_token();
    api.open_model_preview(&AssetAlias::new("gen/drafts/car").unwrap(), "edit", "cage", None).unwrap();
    let mut raw = ts.data(Some(&admin));
    let put = "/v1/model-preview-sessions/edit/parts/body";
    assert_eq!(raw.request("PUT", put, &[], None).status, 411);
    assert_eq!(raw.request("PUT", put, &[], Some(b"")).status, 400);
    assert_eq!(raw.request("PUT", put, &[("Transfer-Encoding", "chunked"), ("Expect", "100-continue")], None).status, 411);
    let mesh = b"draft mesh beyond the durable blob limit";
    let token = api.upload_model_preview_part("edit", "body", &[], &BytesBody::new(mesh, &|| false)).unwrap();
    let path = format!("/v1/model-preview-meshes/{token}");
    let head = raw.head(&path);
    assert_eq!(head.status, 200);
    assert_eq!(head.header("Content-Type"), Some("model/gltf-binary"));
    assert_eq!(head.header("Content-Length").unwrap().parse::<usize>().unwrap(), mesh.len());
    assert!(head.body.is_empty());
    let slice = raw.request("GET", &path, &[("Range", "bytes=2-7")], None);
    assert_eq!(slice.status, 206);
    assert_eq!(slice.body, &mesh[2..8]);
    assert_eq!(slice.header("Content-Range"), Some(format!("bytes 2-7/{}", mesh.len()).as_str()));
    let unsatisfiable = raw.request("GET", &path, &[("Range", "bytes=999-")], None);
    assert_eq!(unsatisfiable.status, 416);
    assert_eq!(unsatisfiable.header("Content-Range"), Some(format!("bytes */{}", mesh.len()).as_str()));

    let mut request = Request::get(&path);
    request.bearer = Some(&admin);
    let response = http_call(ts.server.data_addr(), &request, &HttpLimits::default_v1()).unwrap();
    assert_eq!(response.head().status, 200);
    let next = api.upload_model_preview_part("edit", "body", &[], &BytesBody::new(b"replacement", &|| false)).unwrap();
    assert_eq!(raw.get(&path).status, 404, "replacement withdraws the old token");
    assert_eq!(response.read_full(mesh.len() as u64).unwrap(), mesh, "an opened GET may finish after replacement");
    // Windows keeps a delete-pending name while its live handle is open.
    #[cfg(unix)]
    assert_preview_spool_empty(&ts.root);
    api.update_model_preview("edit", None, &["body".into()], &[]).unwrap();
    assert!(api.fetch_model_preview_mesh(&next, None).is_err());
    assert_preview_spool_empty(&ts.root);
    let token = api.upload_model_preview_part("edit", "body", &[], &BytesBody::new(b"clear me", &|| false)).unwrap();
    api.clear_model_preview("edit").unwrap();
    assert!(api.fetch_model_preview_mesh(&token, None).is_err());
    assert_preview_spool_empty(&ts.root);
    api.open_model_preview(&AssetAlias::new("gen/drafts/car").unwrap(), "restart", "cage", None).unwrap();
    let token = api.upload_model_preview_part("restart", "body", &[], &BytesBody::new(b"restart me", &|| false)).unwrap();
    drop(raw);
    drop(api);
    ts.server.shutdown();
    let root = ts.root.clone();
    drop(ts);
    assert_preview_spool_empty(&root);
    // Simulate a crash in the Unix creation-to-unlink window.
    std::fs::write(root.join("previews/orphan"), b"orphan").unwrap();
    let mut ts = TestServer { server: makepad_asset_store::AssetServer::start(base_config(root.clone())).unwrap(), root };
    assert_preview_spool_empty(&ts.root);
    let api = preview_api(&ts);
    assert!(api.fetch_model_preview_mesh(&token, None).is_err());
    assert!(api.events_page(None, 0, 1, None, &[]).unwrap().events.is_empty());
    drop(api);
    ts.server.shutdown();
    let root = ts.root.clone();
    drop(ts);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn preview_subscriber_bootstraps_again_after_backpressure_retention_gap() {
    use makepad_asset_client::{AssetClient, ApiEndpoints, CatalogSubscriberConfig, CatalogSubscriptionEvent, ClientConfig};
    use makepad_asset_data::AssetAlias;
    let mut ts = start_server_with("preview_resync", |cfg| cfg.event_journal_cap = 2);
    let api = preview_api(&ts);
    let cache = test_root("preview_resync_cache");
    let mut config = ClientConfig::new(cache.clone());
    config.token = Some(ts.admin_token());
    let client = AssetClient::connect(config, ApiEndpoints { control: ts.server.control_addr(), data: ts.server.data_addr() }, Some(ts.server.server_id())).unwrap();
    let mut sub_config = CatalogSubscriberConfig::default_v1();
    sub_config.channel_capacity = 1;
    sub_config.batch_limit = 1;
    sub_config.wait_ms = 100;
    sub_config.preview_audiences = vec!["room-a".into()];
    let mut subscriber = client.subscribe_catalog(sub_config).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if subscriber.poll().iter().any(|e| matches!(e, CatalogSubscriptionEvent::Ready { .. })) { break; }
        assert!(Instant::now() < deadline, "initial subscriber bootstrap");
        std::thread::sleep(Duration::from_millis(5));
    }
    api.open_model_preview(&AssetAlias::new("gen/drafts/car").unwrap(), "edit-car", "initial", Some("room-a")).unwrap();
    // Do not drain: a one-slot queue bounds delivery while the journal rolls
    // over. No edits occur after this loop, so only bootstrap can restore it.
    for id in 0..50 {
        api.update_model_preview("edit-car", Some(&format!("state-{id}")), &[], &[]).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut gap = false;
    let mut restored = false;
    while !restored {
        for event in subscriber.poll() {
            match event {
                CatalogSubscriptionEvent::ResyncRequired { .. } => gap = true,
                CatalogSubscriptionEvent::Events { events, .. } if gap => {
                    restored |= events.iter().any(|e| e.model_preview.as_ref().is_some_and(|p| p.open && p.program.as_deref() == Some("state-49")));
                }
                CatalogSubscriptionEvent::Retry { error, .. } => panic!("subscriber transport error: {error:?}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "gap={gap}, missing cumulative draft bootstrap");
        if !restored { std::thread::sleep(Duration::from_millis(5)); }
    }
    api.open_model_preview(&AssetAlias::new("gen/drafts/other").unwrap(), "edit-other", "other room", Some("room-b")).unwrap();
    subscriber.set_preview_audiences(vec!["room-b".into()]).unwrap();
    let mut scope_reset = false;
    let mut scope_restored = false;
    while !scope_restored {
        for event in subscriber.poll() {
            match event {
                CatalogSubscriptionEvent::ResyncRequired { .. } => scope_reset = true,
                CatalogSubscriptionEvent::Events { events, .. } if scope_reset => {
                    assert!(events.iter().all(|event| event.model_preview.as_ref().is_none_or(|p| p.audience.as_deref() == Some("room-b"))));
                    scope_restored |= events.iter().any(|event| event.model_preview.as_ref().is_some_and(|p| p.open && p.session == "edit-other"));
                }
                CatalogSubscriptionEvent::Retry { error, .. } => panic!("subscriber scope error: {error:?}"),
                _ => {}
            }
        }
        assert!(Instant::now() < deadline, "new audience must bootstrap its already-open draft");
        if !scope_restored { std::thread::sleep(Duration::from_millis(5)); }
    }
    subscriber.shutdown();
    drop(client);
    drop(api);
    ts.server.shutdown();
    let root = ts.root.clone();
    drop(ts);
    std::fs::remove_dir_all(root).unwrap();
    std::fs::remove_dir_all(cache).unwrap();
}

#[test]
fn preview_access_requires_its_own_grant_and_owner_with_moderation_clear() {
    let mut ts = start_server("preview_owner_access");
    let admin = ts.admin_token();
    let mut control = ts.control(Some(&admin));
    let owner_token = principal_with(&mut control, &[("preview_publish", "gen")]);
    let other_token = principal_with(&mut control, &[("preview_publish", "gen"), ("asset_publish", "gen")]);
    let durable_token = principal_with(&mut control, &[("asset_publish", "gen")]);
    let moderator_token = principal_with(&mut control, &[("preview_publish", "gen"), ("asset_quarantine", "gen")]);
    let open = |session: &str| jobj(vec![
        ("op", jstr("open")), ("session", jstr(session)),
        ("alias", jstr(format!("gen/drafts/{session}"))), ("program", jstr("author's draft")),
    ]);
    let delta = jobj(vec![("op", jstr("delta")), ("session", jstr("owned")), ("program", jstr("edited"))]);
    let clear = jobj(vec![("op", jstr("clear")), ("session", jstr("owned"))]);
    let put = "/v1/model-preview-sessions/owned/parts/body";
    let mut owner = ts.control(Some(&owner_token));
    let mut owner_data = ts.data(Some(&owner_token));
    let mut other = ts.control(Some(&other_token));
    let mut other_data = ts.data(Some(&other_token));
    let mut moderator = ts.control(Some(&moderator_token));
    let mut moderator_data = ts.data(Some(&moderator_token));
    let mut durable = ts.control(Some(&durable_token));
    let denied = durable.post_json("/v1/model-previews", &open("durable-only"));
    assert_eq!(denied.status, 403, "durable publication does not grant preview publication");
    assert_eq!(denied.str_field("capability"), "preview_publish");
    assert_eq!(owner.post_json("/v1/model-previews", &open("owned")).status, 200);
    for response in [
        other_data.request("PUT", put, &[], Some(b"foreign")),
        other.post_json("/v1/model-previews", &delta),
        other.post_json("/v1/model-previews", &clear),
        moderator_data.request("PUT", put, &[], Some(b"moderator")),
        moderator.post_json("/v1/model-previews", &delta),
    ] {
        assert_eq!(response.status, 403);
        assert_eq!(response.str_field("error"), "model preview belongs to another principal");
    }
    assert_eq!(owner_data.request("PUT", put, &[], Some(b"owner mesh")).status, 200);
    assert_eq!(owner.post_json("/v1/model-previews", &delta).status, 200);
    assert_eq!(owner.post_json("/v1/model-previews", &clear).status, 200);
    assert_eq!(owner.post_json("/v1/model-previews", &open("owned")).status, 200);
    let token = owner_data.request("PUT", put, &[], Some(b"moderated mesh")).str_field("mesh_token");
    assert_eq!(moderator.post_json("/v1/model-previews", &clear).status, 200, "moderation may clear another principal's draft");
    assert_eq!(owner_data.get(&format!("/v1/model-preview-meshes/{token}")).status, 404);

    let cursor = cursor_of(&control.get("/v1/events?ev=5"));
    let mut admin_data = ts.data(Some(&admin));
    let (asset, revision) = publish_prop_http(&mut control, &mut admin_data, "gen", "gen/durable", b"glb", b"thumb");
    let visible = other.get(&format!("/v1/events?ev=5&cursor={cursor}&preview_audience=unrelated"));
    assert_eq!(events_of(&visible).iter().filter_map(|event| ev_str(event, "kind")).collect::<Vec<_>>(), vec!["asset_published", "alias_set"], "audiences do not filter durable catalog events");
    let denied = owner.post_json(&format!("/v1/assets/{asset}/revisions/{revision}/publish"), &jobj(vec![]));
    assert_eq!(denied.status, 403, "preview publication does not grant durable publication");
    assert_eq!(denied.str_field("capability"), "asset_publish");
    drop((owner, owner_data, other, other_data, moderator, moderator_data, durable, control, admin_data));
    ts.server.shutdown();
    let root = ts.root.clone();
    drop(ts);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn preview_audiences_filter_snapshots_deltas_clears_and_mesh_reads() {
    use makepad_asset_client::ClientError;
    use makepad_asset_data::AssetAlias;
    let mut ts = start_server("preview_audience_access");
    let api = preview_api(&ts);
    let admin = ts.admin_token();
    let mut control = ts.control(Some(&admin));
    let reader_token = principal_with(&mut control, &[]);
    let mut reader = ts.control(Some(&reader_token));
    let mut data = ts.data(Some(&reader_token));
    let start = api.events_page(None, 0, 128, None, &[]).unwrap().cursor;
    let a = "A_.:-0".to_string();
    let b = "B".repeat(128);
    // A filtered-out event before the first match must not consume its page slot.
    for (session, audience) in [("b", Some(b.as_str())), ("a", Some(a.as_str())), ("public", None)] {
        api.open_model_preview(&AssetAlias::new(format!("gen/drafts/{session}")).unwrap(), session, session, audience).unwrap();
    }
    let token = api.upload_model_preview_part("a", "body", &[], &BytesBody::new(b"audience a mesh", &|| false)).unwrap();
    let a_list = std::slice::from_ref(&a);
    let snapshot = api.events_page(None, 0, 1, None, a_list).unwrap();
    let sessions = |page: &makepad_asset_client::EventsPageDto| page.events.iter()
        .filter_map(|event| event.model_preview.as_ref().map(|preview| preview.session.clone()))
        .collect::<Vec<_>>();
    assert_eq!(sessions(&snapshot), vec!["public", "a"], "bootstrap is complete despite the delta page limit");
    let none = api.events_page(None, 0, 128, None, &[]).unwrap();
    assert_eq!(sessions(&none), vec!["public"], "no audience means only unscoped previews");
    let both = reader.get(&format!("/v1/events?ev=5&preview_audience={a},{b}"));
    assert_eq!(both.status, 200);
    assert_eq!(events_of(&both).len(), 3, "a comma-separated audience list includes every named scope");
    assert_eq!(reader.get(&format!("/v1/events?ev=5&preview_audience={a},")).status, 400, "an empty list item is malformed");
    let first = api.events_page(Some(&start), 0, 1, None, a_list).unwrap();
    assert_eq!(sessions(&first), vec!["a"]);
    let rest = api.events_page(Some(&first.cursor), 0, 128, None, a_list).unwrap();
    assert_eq!(sessions(&rest), vec!["public", "a"]);
    assert!(rest.events.iter().all(|event| event.model_preview.as_ref().unwrap().program.as_deref() != Some("b")));
    let unscoped = reader.get(&format!("/v1/events?ev=5&cursor={start}"));
    assert_eq!(events_of(&unscoped).len(), 1);
    assert_eq!(ev_str(&events_of(&unscoped)[0], "preview_session"), Some("public"));
    let path = format!("/v1/model-preview-meshes/{token}");
    for suffix in [String::new(), format!("?audience={b}")] {
        assert_eq!(data.get(&format!("{path}{suffix}")).status, 404);
        assert_eq!(data.head(&format!("{path}{suffix}")).status, 404);
    }
    let fetched = data.get(&format!("{path}?audience={a}"));
    assert_eq!(fetched.status, 200);
    assert_eq!(fetched.body, b"audience a mesh");
    assert_eq!(api.fetch_model_preview_mesh(&token, Some(&a)).unwrap(), b"audience a mesh");
    let cursor = snapshot.cursor;
    api.update_model_preview("b", Some("private b program"), &[], &[]).unwrap();
    api.update_model_preview("a", Some("private a program"), &[], &[]).unwrap();
    api.clear_model_preview("b").unwrap();
    api.clear_model_preview("a").unwrap();
    let deltas = api.events_page(Some(&cursor), 0, 128, None, a_list).unwrap();
    assert_eq!(sessions(&deltas), vec!["a", "a"]);
    assert!(deltas.events.iter().all(|event| event.model_preview.as_ref().unwrap().audience.as_deref() == Some(a.as_str())));
    assert_eq!(deltas.events[1].kind, makepad_asset_client::CatalogEventKind::ModelPreviewClear);
    let hidden = api.events_page(Some(&cursor), 0, 128, None, &[]).unwrap();
    assert!(hidden.events.is_empty());
    assert_eq!(hidden.cursor, deltas.cursor, "a filtered poll still advances past all hidden events");

    for invalid in ["".to_string(), "a".repeat(129), "a/b".into(), "é".into()] {
        assert!(matches!(api.open_model_preview(&AssetAlias::new("gen/drafts/invalid").unwrap(), "invalid", "", Some(&invalid)), Err(ClientError::InvalidInput { .. })));
        assert!(matches!(api.events_page(None, 0, 1, None, std::slice::from_ref(&invalid)), Err(ClientError::InvalidInput { .. })));
        assert!(matches!(api.fetch_model_preview_mesh(&token, Some(&invalid)), Err(ClientError::InvalidInput { .. })));
        let response = control.post_json("/v1/model-previews", &jobj(vec![
            ("op", jstr("open")), ("session", jstr("invalid")), ("alias", jstr("gen/drafts/invalid")),
            ("program", jstr("")), ("audience", jstr(invalid)),
        ]));
        assert_eq!(response.status, 400);
    }
    assert_eq!(reader.get("/v1/events?ev=5&preview_audience=").status, 400);
    assert_eq!(reader.get("/v1/events?ev=5&ev=5").status, 400, "scalar query parameters still reject duplicates");
    assert_eq!(data.get(&format!("{path}?audience={a}&audience={b}")).status, 400);
    assert_eq!(data.get(&format!("{path}?audience=")).status, 400);
    drop((api, control, reader, data));
    ts.server.shutdown();
    let root = ts.root.clone();
    drop(ts);
    std::fs::remove_dir_all(root).unwrap();
}

/// Publish a prop whose annotation declares `kind`, and return its ids.
fn publish_annotated(
    control: &mut Client,
    data: &mut Client,
    ns: &str,
    alias: &str,
    seed: u8,
    kind: &str,
) -> (String, String) {
    let glb = vec![seed; 600];
    let thumb = vec![seed ^ 0xff; 300];
    let (asset_id, revision) = publish_prop_http(control, data, ns, alias, &glb, &thumb);
    let r = control.put_json(
        &format!("/v1/assets/{asset_id}/annotation"),
        &jobj(vec![
            ("title", jstr(format!("clip {seed}"))),
            ("kind", jstr(kind)),
        ]),
    );
    assert_eq!(r.status, 204, "{}", String::from_utf8_lossy(&r.body));
    (asset_id, revision)
}

#[test]
fn publish_and_alias_emit_events_with_cursor_resume_and_no_duplicates() {
    let ts = start_server("events_publish");
    let admin = ts.admin_token();
    let (mut control, mut data) = content_clients(&ts, &admin, "stock");

    // Tail resume point before any activity.
    let r = control.get("/v1/events");
    assert_eq!(r.status, 200);
    assert!(events_of(&r).is_empty());
    assert!(!gap_of(&r));
    let start_cursor = cursor_of(&r);

    let (asset_id, revision) =
        publish_annotated(&mut control, &mut data, "stock", "stock/clip-a", 1, "video");

    // Everything since the resume point: publish, annotation, alias.
    let r = control.get(&format!("/v1/events?cursor={start_cursor}"));
    assert_eq!(r.status, 200);
    let events = events_of(&r);
    let kinds: Vec<&str> = events.iter().filter_map(|e| ev_str(e, "kind")).collect();
    assert_eq!(kinds, vec!["asset_published", "alias_set", "annotation_set"]);
    assert!(!gap_of(&r));
    // The publish event names the exact asset/revision identity.
    assert_eq!(ev_str(&events[0], "asset_id"), Some(asset_id.as_str()));
    assert_eq!(ev_str(&events[0], "revision"), Some(revision.as_str()));
    assert_eq!(ev_str(&events[0], "ns"), Some("stock"));
    assert!(events[0].get("ts_ms").and_then(Value::as_u64).is_some());
    // The alias event carries the alias and (annotation not yet set at that
    // point) no content kind; the annotation event carries the kind.
    assert_eq!(ev_str(&events[1], "alias"), Some("stock/clip-a"));
    assert_eq!(ev_str(&events[2], "content_kind"), Some("video"));
    // Sequences strictly increase.
    let seqs: Vec<u64> = events.iter().filter_map(|e| e.get("seq").and_then(Value::as_u64)).collect();
    assert!(seqs.windows(2).all(|w| w[0] < w[1]), "{seqs:?}");

    // Resuming from the returned cursor replays nothing.
    let done_cursor = cursor_of(&r);
    let r = control.get(&format!("/v1/events?cursor={done_cursor}"));
    assert!(events_of(&r).is_empty());
    assert!(!gap_of(&r));
    assert_eq!(cursor_of(&r), done_cursor);

    // A malformed cursor is refused, not misread.
    let r = control.get("/v1/events?cursor=garbage");
    assert_eq!(r.status, 400);
}

#[test]
fn kind_filter_selects_video_and_advances_past_foreign_events() {
    let ts = start_server("events_filter");
    let admin = ts.admin_token();
    let (mut control, mut data) = content_clients(&ts, &admin, "stock");

    let start = cursor_of(&control.get("/v1/events"));
    let (video_asset, _) =
        publish_annotated(&mut control, &mut data, "stock", "stock/vid", 11, "video");
    let (_mesh_asset, _) =
        publish_annotated(&mut control, &mut data, "stock", "stock/mesh", 12, "mesh");

    let r = control.get(&format!("/v1/events?cursor={start}&kind=video"));
    let events = events_of(&r);
    // Publish/alias events precede their annotation, so their content kind is
    // unknown (conservatively included); the mesh ANNOTATION event, whose
    // kind is known, is filtered out. Every surviving known kind is video.
    assert!(!events.is_empty());
    for ev in &events {
        if let Some(ck) = ev_str(ev, "content_kind") {
            assert_eq!(ck, "video");
        }
    }
    assert!(events.iter().any(|e| {
        ev_str(e, "kind") == Some("annotation_set")
            && ev_str(e, "asset_id") == Some(video_asset.as_str())
    }));
    assert!(!events.iter().any(|e| ev_str(e, "content_kind") == Some("mesh")));
    // The cursor advanced past the filtered-out mesh events: replaying from
    // it returns nothing new.
    let cursor = cursor_of(&r);
    let r = control.get(&format!("/v1/events?cursor={cursor}&kind=video"));
    assert!(events_of(&r).is_empty());

    // After annotation, a new publish of the SAME asset carries its kind, so
    // an exact-kind subscriber sees it even under the filter.
    let glb = vec![21u8; 700];
    let thumb = vec![22u8; 300];
    let ast: AssetId = video_asset.parse().unwrap();
    let bytes = prop_manifest(ast, &glb, &thumb).to_canonical_bytes().unwrap();
    for b in [&glb, &thumb] {
        let r = data.post_bytes("/v1/blobs?ns=stock", b);
        assert_eq!(r.status, 201);
    }
    let r = control.post_bytes(&format!("/v1/assets/{video_asset}/revisions"), &bytes);
    assert_eq!(r.status, 201);
    let rev2 = r.str_field("revision");
    let r = control.post_json(
        &format!("/v1/assets/{video_asset}/revisions/{rev2}/publish"),
        &jobj(vec![]),
    );
    assert_eq!(r.status, 200);
    let r = control.get(&format!("/v1/events?cursor={cursor}&kind=video"));
    let events = events_of(&r);
    assert_eq!(events.len(), 1);
    assert_eq!(ev_str(&events[0], "kind"), Some("asset_published"));
    assert_eq!(ev_str(&events[0], "content_kind"), Some("video"));
    assert_eq!(ev_str(&events[0], "revision"), Some(rev2.as_str()));

    // Unknown kind value is refused.
    let r = control.get(&format!("/v1/events?cursor={cursor}&kind=blob"));
    assert_eq!(r.status, 400);
}

#[test]
fn long_poll_wakes_on_publish_and_times_out_empty() {
    let ts = start_server("events_longpoll");
    let admin = ts.admin_token();
    let (mut control, mut data) = content_clients(&ts, &admin, "stock");
    let start = cursor_of(&control.get("/v1/events"));

    // Timeout path: nothing happens; the poll parks for its wait and answers empty. (No upper
    // bound on the wall time: on a busy box a 5 s one failed a poll that woke, 11-22.)
    let t0 = Instant::now();
    let r = control.get(&format!("/v1/events?cursor={start}&wait=300"));
    assert_eq!(r.status, 200);
    assert!(events_of(&r).is_empty());
    assert!(!gap_of(&r));
    let waited = t0.elapsed();
    assert!(waited >= Duration::from_millis(250), "waited {waited:?}");

    // Wakeup path: a parked poll answers with the publish. A poll whose wait runs out answers
    // empty (the route reads again only once woken), so the published event in the answer is the
    // wake, however long a loaded box took over the publish: the wait is the server's longest.
    let addr = ts.server.control_addr();
    let admin_t = admin.clone();
    let start_t = start.clone();
    let waiter = std::thread::spawn(move || {
        let mut c = Client::new(addr, Some(&admin_t));
        let t0 = Instant::now();
        let r = c.get(&format!("/v1/events?cursor={start_t}&wait=30000"));
        (r, t0.elapsed())
    });
    // Give the waiter time to park, then publish.
    std::thread::sleep(Duration::from_millis(200));
    let (asset_id, revision) = publish_annotated(&mut control, &mut data, "stock", "stock/wake", 31, "video");
    let (r, waited) = waiter.join().unwrap();
    assert_eq!(r.status, 200);
    let events = events_of(&r);
    assert!(!events.is_empty(), "long poll should wake on publish, answered empty after {waited:?}");
    assert_eq!(ev_str(&events[0], "asset_id"), Some(asset_id.as_str()), "the wake answers with the publish");
    assert_eq!(ev_str(&events[0], "revision"), Some(revision.as_str()));
}

#[test]
fn retention_gap_forces_resync() {
    // Tiny journal: publishing more events than it retains invalidates old
    // cursors explicitly.
    let ts = start_server_with("events_gap", |cfg| {
        cfg.event_journal_cap = 4;
    });
    let admin = ts.admin_token();
    let (mut control, mut data) = content_clients(&ts, &admin, "stock");

    let start = cursor_of(&control.get("/v1/events"));
    // Each publish emits 3 events (publish, alias, annotation); 3 rounds
    // overflow a cap of 4.
    for seed in 41..44u8 {
        publish_annotated(
            &mut control,
            &mut data,
            "stock",
            &format!("stock/gap-{seed}"),
            seed,
            "video",
        );
    }
    let r = control.get(&format!("/v1/events?cursor={start}"));
    assert_eq!(r.status, 200);
    assert!(gap_of(&r), "evicted cursor must report a gap");
    assert!(events_of(&r).is_empty());
    // The gap cursor is a clean tail resume point.
    let cursor = cursor_of(&r);
    let r = control.get(&format!("/v1/events?cursor={cursor}"));
    assert!(!gap_of(&r));
    assert!(events_of(&r).is_empty());

    // A cursor from another journal life (foreign epoch) is a gap too.
    let foreign = format!("{}-1", "ab".repeat(8));
    let r = control.get(&format!("/v1/events?cursor={foreign}"));
    assert_eq!(r.status, 200);
    assert!(gap_of(&r));
}

#[test]
fn batch_limit_pages_without_loss() {
    let ts = start_server("events_batch");
    let admin = ts.admin_token();
    let (mut control, mut data) = content_clients(&ts, &admin, "stock");
    let start = cursor_of(&control.get("/v1/events"));
    for seed in 51..54u8 {
        publish_annotated(
            &mut control,
            &mut data,
            "stock",
            &format!("stock/batch-{seed}"),
            seed,
            "audio",
        );
    }
    // 9 events total; page through 2 at a time and prove exact-once order.
    let mut cursor = start;
    let mut seqs: Vec<u64> = Vec::new();
    loop {
        let r = control.get(&format!("/v1/events?cursor={cursor}&limit=2"));
        assert_eq!(r.status, 200);
        let events = events_of(&r);
        assert!(events.len() <= 2);
        cursor = cursor_of(&r);
        if events.is_empty() {
            break;
        }
        seqs.extend(events.iter().filter_map(|e| e.get("seq").and_then(Value::as_u64)));
    }
    assert_eq!(seqs.len(), 9);
    assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1), "{seqs:?}");
}

#[test]
fn events_require_auth_and_refuse_bad_params() {
    let ts = start_server("events_auth");
    let admin = ts.admin_token();
    let mut control = ts.control(Some(&admin));

    // No token, garbage token: the uniform 401.
    let mut anon = ts.control(None);
    assert_eq!(anon.get("/v1/events").status, 401);
    let bogus = format!("mpat_{}", "ab".repeat(32));
    let mut bad = ts.control(Some(&bogus));
    assert_eq!(bad.get("/v1/events").status, 401);

    // Authenticated: parameter refusals are explicit 400s.
    assert_eq!(control.get("/v1/events?wait=abc").status, 400);
    assert_eq!(control.get("/v1/events?limit=0").status, 400);
    assert_eq!(control.get("/v1/events?kind=nope").status, 400);
    assert_eq!(control.get("/v1/events?cursor=zz-1").status, 400);
    // Method discipline: POST is not a read.
    let r = control.post_json("/v1/events", &jobj(vec![]));
    assert_eq!(r.status, 405);
}

#[test]
fn shutdown_releases_parked_long_polls_promptly() {
    let ts = start_server("events_shutdown");
    let admin = ts.admin_token();
    let mut control = ts.control(Some(&admin));
    let start = cursor_of(&control.get("/v1/events"));

    // Park three long-polls (the "slow clients"), then shut the server down.
    let mut waiters = Vec::new();
    for _ in 0..3 {
        let addr = ts.server.control_addr();
        let token = admin.clone();
        let cursor = start.clone();
        waiters.push(std::thread::spawn(move || {
            let mut c = Client::new(addr, Some(&token));
            let t0 = Instant::now();
            let r = c.get(&format!("/v1/events?cursor={cursor}&wait=30000"));
            (r.status, t0.elapsed())
        }));
    }
    std::thread::sleep(Duration::from_millis(300));
    let mut server = ts;
    let t0 = Instant::now();
    server.server.shutdown();
    let shutdown_took = t0.elapsed();
    assert!(
        shutdown_took < Duration::from_secs(10),
        "shutdown must not wait out 30s long-polls, took {shutdown_took:?}"
    );
    for w in waiters {
        let (status, waited) = w.join().unwrap();
        // Parked polls answer with an empty batch before the socket closes.
        assert_eq!(status, 200);
        assert!(waited < Duration::from_secs(10), "waited {waited:?}");
    }
}

#[test]
fn parked_waiters_beyond_cap_degrade_to_immediate_polls() {
    let ts = start_server_with("events_waiter_cap", |cfg| {
        cfg.event_max_waiters = 1;
    });
    let admin = ts.admin_token();
    let mut control = ts.control(Some(&admin));
    let start = cursor_of(&control.get("/v1/events"));

    // First waiter parks and holds the only slot.
    let addr = ts.server.control_addr();
    let token = admin.clone();
    let cursor = start.clone();
    let parked = std::thread::spawn(move || {
        let mut c = Client::new(addr, Some(&token));
        c.get(&format!("/v1/events?cursor={cursor}&wait=2000")).status
    });
    std::thread::sleep(Duration::from_millis(200));
    // Over the cap: this poll answers immediately instead of parking.
    let t0 = Instant::now();
    let r = control.get(&format!("/v1/events?cursor={start}&wait=2000"));
    assert_eq!(r.status, 200);
    assert!(events_of(&r).is_empty());
    assert!(
        t0.elapsed() < Duration::from_millis(1500),
        "over-cap poll must not park, took {:?}",
        t0.elapsed()
    );
    assert_eq!(parked.join().unwrap(), 200);
}

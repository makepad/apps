#![cfg(not(target_arch = "wasm32"))]

use makepad_asset_client::ApiEndpoints;
use makepad_flow::engine::executors::lineage::{GeneratorRecord, Lineage};
use makepad_flow::engine::executors::publish::{PublishExecutor, PublishRun};
use makepad_flow_assets::{AssetListQuery, AssetStoreConfig, AssetWorker, StoreLocator};
use makepad_flow::engine::executors::{Executor, Poll};
use makepad_flow::{Literal, Loc, Node, Port, PortType, Value};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use makepad_flow::PortValue;

struct Roots(Vec<PathBuf>);

impl Roots {
    fn new() -> Self { Self(Vec::new()) }

    fn make(&mut self, label: &str) -> PathBuf {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("flow-publish-{}-{label}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        self.0.push(path.clone());
        path
    }
}

impl Drop for Roots {
    fn drop(&mut self) {
        for path in &self.0 { let _ = std::fs::remove_dir_all(path); }
    }
}

fn start_store(root: PathBuf) -> (makepad_asset_store::AssetServer, String) {
    let mut config = makepad_asset_store::ServerConfig::new(root.clone());
    config.control_addr = "127.0.0.1:0".parse().unwrap();
    config.data_addr = "127.0.0.1:0".parse().unwrap();
    config.bootstrap_admin = true;
    config.log = false;
    let server = makepad_asset_store::AssetServer::start(config).unwrap();
    let token = std::fs::read_to_string(root.join("admin-token")).unwrap().trim().to_string();
    (server, token)
}

fn node() -> Node {
    Node {
        id: "library".into(),
        kind: "publish".into(),
        type_name: "Publish".into(),
        params: vec![
            ("title".into(), Literal::Str("Sunset result".into())),
            ("namespace".into(), Literal::Str("flows".into())),
            ("tags".into(), Literal::Arr(vec![Literal::Str("flow".into()), Literal::Str("sunset".into())])),
            ("description".into(), Literal::Str("painted from a prompt".into())),
            ("alias".into(), Literal::Str("flows/sunset-result".into())),
        ],
        inputs: Vec::new(),
        outputs: vec![Port { name: "asset".into(), ty: PortType::Json, many: None }],
        at: None,
        size: None,
        flip: false,
        loc: Loc::default(),
        fn_src: None,
        face_src: None,
        on_fail: "fail".into(),
        label: None,
        domain: None,
        doc: None,
    }
}

fn picture(fill: u8) -> Value {
    let mut pixels = vec![fill; 256 * 256 * 4];
    for alpha in pixels.iter_mut().skip(3).step_by(4) { *alpha = 255; }
    let png = makepad_ai_hub::testpattern::encode_png_rgba(&pixels, 256, 256).unwrap();
    Value::media(PortType::Image, "image/png", png)
}

fn publish(worker: &AssetWorker, value: Value) -> makepad_strict_json::Value {
    let run = PublishRun { flow: "prompt-to-library".into(), instance: "instance-1".into(), ..Default::default() };
    publish_node(worker, &node(), run, value)
}

fn publish_node(worker: &AssetWorker, node: &Node, run: PublishRun, value: Value) -> makepad_strict_json::Value {
    let mut executor = PublishExecutor::new(Some(worker.store()), run);
    executor.start(node, &[("value".into(), PortValue::One(value))]).unwrap();
    loop {
        match executor.poll() {
            Poll::Done(outputs) => {
                let value = outputs.into_iter().find(|(port, _)| port == "asset").unwrap().1.one().cloned().unwrap();
                return makepad_strict_json::parse(&value.bytes).unwrap();
            }
            Poll::Failed(error) => panic!("publish failed: {error}"),
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// A store that moves (its host quit and another process hosts the same
/// root on new ports): a worker with a locator follows it, and a publish
/// made before the move lists after it.
#[test]
fn a_located_worker_follows_its_store_to_a_new_host() {
    let mut roots = Roots::new();
    let root = roots.make("store");
    let (mut first, token) = start_store(root.clone());
    let at = std::sync::Arc::new(std::sync::Mutex::new((
        ApiEndpoints { control: first.control_addr(), data: first.data_addr() },
        first.server_id(),
        token.clone(),
    )));
    let now = at.clone();
    let mut config = AssetStoreConfig::new(roots.make("cache"));
    config.discovery_wait_ms = 0;
    config.locate = Some(StoreLocator(std::sync::Arc::new(move || Ok(now.lock().unwrap().clone()))));
    let mut worker = AssetWorker::start(config).unwrap();
    publish(&worker, picture(30));
    let list = |worker: &AssetWorker| worker.handle().list(AssetListQuery { text: String::new(), namespace: Some("flows".into()), limit: 10, cursor: None });
    assert_eq!(list(&worker).unwrap().len(), 1);

    let id = first.server_id();
    first.shutdown();
    drop(first);
    let (second, token) = start_store(root);
    assert_eq!(second.server_id(), id, "the same store");
    *at.lock().unwrap() = (ApiEndpoints { control: second.control_addr(), data: second.data_addr() }, second.server_id(), token);
    let rows = list(&worker).expect("the worker follows the store to its new host");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Sunset result");
    worker.stop();
}

#[test]
fn picture_publish_and_alias_republish_create_revisions() {
    let mut roots = Roots::new();
    let (mut server, token) = start_store(roots.make("store"));
    let config = AssetStoreConfig {
        archive_outputs: false,
        cache_dir: roots.make("cache"),
        token: Some(token),
        endpoints: Some(ApiEndpoints { control: server.control_addr(), data: server.data_addr() }),
        server_id: Some(server.server_id()),
        discovery_port: 0,
        discovery_wait_ms: 0,
        locate: None,
        namespace: "flows".into(),
    };
    let mut worker = AssetWorker::start(config).unwrap();
    let first = publish(&worker, picture(30));
    let second = publish(&worker, picture(90));
    assert_eq!(first.get("id").and_then(|v| v.as_str()), second.get("id").and_then(|v| v.as_str()));
    assert_ne!(first.get("revision").and_then(|v| v.as_str()), second.get("revision").and_then(|v| v.as_str()));
    assert_eq!(second.get("alias").and_then(|v| v.as_str()), Some("flows/sunset-result"));
    assert_eq!(second.get("namespace").and_then(|v| v.as_str()), Some("flows"));
    assert_eq!(second.get("title").and_then(|v| v.as_str()), Some("Sunset result"));
    assert_eq!(second.get("kind").and_then(|v| v.as_str()), Some("texture"));

    let rows = worker.handle().list(AssetListQuery { text: String::new(), namespace: Some("flows".into()), limit: 10, cursor: None }).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "Sunset result");
    assert_eq!(rows[0].namespace, "flows");
    assert_eq!(rows[0].tags, vec!["flow", "sunset"]);
    worker.stop();
    server.shutdown();
}

/// A reference that names a revision reads exactly that revision (content
/// and thumbnail); one without reads the newest publication.
#[test]
fn a_result_ref_with_a_revision_reads_that_revision() {
    use makepad_flow::engine::results::ResultRef;
    let mut roots = Roots::new();
    let (mut server, token) = start_store(roots.make("store"));
    let config = AssetStoreConfig {
        archive_outputs: false,
        cache_dir: roots.make("cache"),
        token: Some(token),
        endpoints: Some(ApiEndpoints { control: server.control_addr(), data: server.data_addr() }),
        server_id: Some(server.server_id()),
        discovery_port: 0,
        discovery_wait_ms: 0,
        locate: None,
        namespace: "flows".into(),
    };
    let mut worker = AssetWorker::start(config).unwrap();
    let first = publish(&worker, picture(30));
    let second = publish(&worker, picture(90));
    let id = first.get("id").and_then(|v| v.as_str()).unwrap().to_string();
    let revision = |published: &makepad_strict_json::Value| published.get("revision").and_then(|v| v.as_str()).unwrap().to_string();
    let store = worker.store();
    let at = |revision: Option<String>| ResultRef { id: id.clone(), revision, alias: None };
    let old = store.read(&at(Some(revision(&first)))).unwrap();
    let new = store.read(&at(Some(revision(&second)))).unwrap();
    let head = store.read(&at(None)).unwrap();
    assert_ne!(old.bytes, new.bytes, "two revisions, two pictures");
    assert_eq!(head.bytes, new.bytes, "no revision reads the newest");
    let old_thumb = store.thumbnail(&at(Some(revision(&first)))).unwrap().unwrap();
    let new_thumb = store.thumbnail(&at(Some(revision(&second)))).unwrap().unwrap();
    assert_ne!(old_thumb.bytes, new_thumb.bytes, "each revision's own thumbnail");
    assert!(store.read(&at(Some("not-a-revision".into()))).is_err());
    worker.stop();
    server.shutdown();
}

#[test]
fn a_derived_publish_records_its_generator_prompt_and_library_parent() {
    let mut roots = Roots::new();
    let (mut server, token) = start_store(roots.make("store"));
    let endpoints = ApiEndpoints { control: server.control_addr(), data: server.data_addr() };
    let config = AssetStoreConfig {
        archive_outputs: false,
        cache_dir: roots.make("cache"),
        token: Some(token.clone()),
        endpoints: Some(endpoints),
        server_id: Some(server.server_id()),
        discovery_port: 0,
        discovery_wait_ms: 0,
        locate: None,
        namespace: "flows".into(),
    };
    let mut worker = AssetWorker::start(config).unwrap();
    // A library picture, as a host would feed it to a run.
    let source = picture(40);
    let first = publish(&worker, source.clone());
    let source_id: makepad_asset_data::AssetId = first.get("id").and_then(|v| v.as_str()).unwrap().parse().unwrap();
    let source_revision: makepad_asset_data::AssetRevisionId =
        first.get("revision").and_then(|v| v.as_str()).unwrap().parse().unwrap();

    let mut derived = node();
    // No tags: the defaults, `flow` and the flow's name, as labels, plus the
    // upscale product's own tag.
    derived.params.retain(|(name, _)| name != "alias" && name != "tags");
    derived.params.push(("bpm".into(), Literal::Num(124.0)));
    derived.params.push(("artist".into(), Literal::Str("Stage".into())));
    let run = PublishRun {
        flow: "studio.Upscale".into(),
        instance: "i".into(),
        run: "run-1".into(),
        revision: 3,
        description_default: String::new(),
        lineage: Lineage {
            generators: vec![GeneratorRecord {
                node: "upscale".into(),
                type_name: "Upscale".into(),
                domain: "upscale".into(),
                model: "realesrgan".into(),
                seed: Some(42),
                params: vec![("factor".into(), Literal::Num(2.0))],
                hub: String::new(),
            }],
            prompt: "a sunset".into(),
            sources: vec![source.digest],
        },
    };
    let second = publish_node(&worker, &derived, run, picture(200));
    let id: makepad_asset_data::AssetId = second.get("id").and_then(|v| v.as_str()).unwrap().parse().unwrap();
    assert_ne!(id, source_id);

    let rows = worker.handle().list(AssetListQuery { text: String::new(), namespace: Some("flows".into()), limit: 10, cursor: None }).unwrap();
    let row = rows.iter().find(|row| row.id == id.to_string()).unwrap();
    let mut tags = row.tags.clone();
    tags.sort();
    assert_eq!(tags, ["bpm-124", "flow", "studio-upscale", "upscaled"]);

    let mut client = makepad_asset_client::AssetClient::connect(
        makepad_asset_client::ClientConfig { token: Some(token), ..makepad_asset_client::ClientConfig::new(roots.make("reader")) },
        endpoints,
        Some(server.server_id()),
    )
    .unwrap();
    let detail = client.asset_detail(&id).unwrap();
    let revision = detail.latest_published().unwrap().revision;
    let manifest = client.fetch_asset_manifest(&revision).unwrap();
    let provenance = manifest.provenance.expect("typed provenance");
    assert_eq!((provenance.generator.as_str(), provenance.model.as_str(), provenance.seed), ("makepad-flow", "realesrgan", 42));
    assert_eq!(provenance.parents, [source_revision]);
    assert!(provenance.params_digest.is_some());
    worker.stop();
    server.shutdown();
}

#[test]
fn portrait_landscape_and_banner_pictures_publish() {
    let mut roots = Roots::new();
    let (mut server, token) = start_store(roots.make("store"));
    let config = AssetStoreConfig {
        archive_outputs: false,
        cache_dir: roots.make("cache"),
        token: Some(token),
        endpoints: Some(ApiEndpoints { control: server.control_addr(), data: server.data_addr() }),
        server_id: Some(server.server_id()),
        discovery_port: 0,
        discovery_wait_ms: 0,
        locate: None,
        namespace: "flows".into(),
    };
    let mut worker = AssetWorker::start(config).unwrap();
    let mut plain = node();
    plain.params.retain(|(name, _)| name != "alias");
    // Flux 9:16 and 16:9, a square, and a banner past 4:1 either way.
    for (width, height) in [(1088, 1920), (1920, 1088), (1024, 1024), (2048, 128), (128, 2048)] {
        let mut pixels = vec![120; width * height * 4];
        for alpha in pixels.iter_mut().skip(3).step_by(4) { *alpha = 255; }
        let png = makepad_ai_hub::testpattern::encode_png_rgba(&pixels, width, height).unwrap();
        let run = PublishRun { flow: "flux".into(), instance: "i".into(), ..Default::default() };
        let published = publish_node(&worker, &plain, run, Value::media(PortType::Image, "image/png", png));
        assert_eq!(published.get("kind").and_then(|v| v.as_str()), Some("texture"), "{width}x{height}");
    }
    worker.stop();
    server.shutdown();
}

#[test]
fn missing_store_failure_names_discovery() {
    let mut roots = Roots::new();
    let mut config = AssetStoreConfig::new(roots.make("cache"));
    config.discovery_wait_ms = 0;
    let mut worker = AssetWorker::start(config).unwrap();
    let run = PublishRun { flow: "demo".into(), instance: "one".into(), ..Default::default() };
    let mut executor = PublishExecutor::new(Some(worker.store()), run);
    executor.start(&node(), &[("value".into(), PortValue::One(picture(10)))]).unwrap();
    loop {
        match executor.poll() {
            Poll::Failed(error) => {
                assert!(error.contains("no asset server discovered on this LAN"), "{error}");
                break;
            }
            Poll::Done(_) => panic!("publish without a store must fail"),
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    worker.stop();
}

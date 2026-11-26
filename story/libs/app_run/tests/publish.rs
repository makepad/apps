//! A counter run published into a real store on loopback: the script as an
//! `AppRun` asset, the MP4 and run.json as its render, and the freshness
//! check. One test function (one app instance at a time).

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig};
use makepad_asset_data::{AssetKind, AssetRevisionRef, FileRole, MediaType};
use makepad_asset_store::{AssetServer, ServerConfig};
use makepad_story_app_run::*;
use makepad_strict_json::Value;

fn temp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("makepad-app-run-publish-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

const RUN: &str = "use mod.apprun.*

AppRun{
    label: \"Counter: one click\"
    app: \"counter\"
    output: {width: 1080 height: 1920 dpi_scale: 3 fps: 60}
    script: fn(app) {
        app.click(@increment_button)
        app.mark(@clicked)
        app.wide({hold: 0.5})
        app.hold(0.5)
    }
}
";

#[test]
fn a_run_publishes_as_an_app_run_with_its_render_and_knows_when_it_is_stale() {
    let root = temp("store");
    let mut cfg = ServerConfig::new(root.clone());
    cfg.control_addr = "127.0.0.1:0".parse().unwrap();
    cfg.data_addr = "127.0.0.1:0".parse().unwrap();
    cfg.bootstrap_admin = true;
    cfg.log = false;
    let mut server = AssetServer::start(cfg).expect("server start");
    let token = std::fs::read_to_string(root.join("admin-token")).unwrap().trim().to_string();
    let mut config = ClientConfig::new(temp("cache"));
    config.token = Some(token);
    let endpoints = ApiEndpoints { control: server.control_addr(), data: server.data_addr() };
    let mut client = AssetClient::connect(config, endpoints, Some(server.server_id())).expect("connect");

    let mut opts = RunOptions::new(temp("run"));
    opts.thumbnail = true;
    let mut result = execute(RUN, opts).expect("run");
    assert!(result.ok(), "{:?}", result.failures);
    let png = result.thumbnail.clone().expect("thumbnail");
    assert!(png.starts_with(b"\x89PNG"));

    let published = publish_run(&mut client, "demo", None, RUN, "Counter: one click", &mut result).expect("publish");

    // The script: an AppRun revision with its source and no dependencies.
    let manifest = client.fetch_asset_manifest(&published.run.revision).expect("run manifest");
    assert_eq!(manifest.kind, AssetKind::AppRun);
    assert!(manifest.dependencies.is_empty());
    let source = manifest.files.iter().find(|f| f.role == FileRole::Source).expect("source file");
    assert_eq!(source.media, MediaType::Text);
    assert_eq!(client.fetch_blob_bytes(&source.blob, None).unwrap(), RUN.as_bytes());

    // The render: a Video pinned to exactly that revision, found by alias.
    assert_eq!(published.alias.as_deref(), Some(format!("demo/render/{}", published.run.asset_id).as_str()));
    assert_eq!(client.render_of(&published.run.asset_id).unwrap(), Some((published.render, published.run)));
    let render = client.fetch_asset_manifest(&published.render.revision).expect("render manifest");
    assert_eq!(render.kind, AssetKind::Video);
    assert_eq!(render.dependencies, vec![published.run]);
    let video = render.files.iter().find(|f| f.role == FileRole::Video).unwrap();
    assert_eq!(client.fetch_blob_bytes(&video.blob, None).unwrap(), std::fs::read(&result.mp4).unwrap());
    let run_json = render.files.iter().find(|f| f.role == FileRole::Source && f.media == MediaType::Json).unwrap();
    let uploaded = client.fetch_blob_bytes(&run_json.blob, None).unwrap();
    let record = makepad_strict_json::parse_depth(&uploaded, 32).unwrap();
    let head = published.run.revision.to_string();
    assert_eq!(record.get("run_revision").and_then(Value::as_str), Some(head.as_str()));
    assert_eq!(record.get("marks").and_then(|m| m.get("clicked")).and_then(Value::as_u64), Some(25));
    let frames = record.get("focus").and_then(|f| f.get("frames")).and_then(Value::as_arr).unwrap();
    assert!(frames.iter().any(|f| f.to_json().contains("\"wide\"")));

    // Fresh against its own revision and the binary as built, unless other
    // work in this checkout changed the sources since (the key hashes
    // uncommitted changes too); a new script revision makes it stale.
    // Nothing re-runs.
    let now = makepad_story_app_run::publish::current_key(&record, &head).unwrap();
    let recorded = record.get("cache_key").and_then(Value::as_str).unwrap().to_string();
    let trees = |b: &Value| (b.get("repo_diff_sha256").map(Value::to_json), b.get("makepad_diff_sha256").map(Value::to_json));
    let (entry, root, binary) = makepad_story_app_run::identity::locate("counter").unwrap();
    let current = BuildIdentity::current(entry, &root, &binary, true).unwrap().to_json();
    if trees(record.get("build").unwrap()) == trees(&current) {
        assert_eq!(now, recorded);
        assert!(!stale(&record, &head).unwrap());
    }
    let other = AssetRevisionRef { asset_id: published.run.asset_id, revision: published.render.revision };
    assert!(stale(&record, &other.revision.to_string()).unwrap());

    // Publishing again against that head: the unchanged script is reused,
    // only a new render is made.
    let again = publish_run(&mut client, "demo", Some(published.run), RUN, "Counter: one click", &mut result).expect("republish");
    assert_eq!(again.run, published.run);
    assert_ne!(again.render, published.render);
    // A changed script is a new revision of the same asset.
    let edited = RUN.replace("hold(0.5)\n    }", "hold(0.5)\n        app.hold(0.1)\n    }");
    let changed = publish_run(&mut client, "demo", Some(published.run), &edited, "Counter: one click", &mut result).expect("edited");
    assert_eq!(changed.run.asset_id, published.run.asset_id);
    assert_ne!(changed.run.revision, published.run.revision);

    // A failed run is not published.
    result.failures.push("click: made up".into());
    assert!(publish_run(&mut client, "demo", None, RUN, "x", &mut result).unwrap_err().message.contains("failed run"));
    server.shutdown();
}

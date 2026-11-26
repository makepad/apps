//! A Graphic's Motion document from a real library (over sockets): the
//! store resolver reads a published `Motion` asset's source the way the
//! Stage host's resolver does, and the graphics' sources resolve for the
//! renderer.

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetKind, FileRole, MediaType, ThumbnailMedia};
use makepad_asset_store::{AssetServer, ServerConfig};
use makepad_story_edit_render::graphic::resolve_sources;
use makepad_story_edit_render::publish::thumbnail_jpeg;
use makepad_story_edit_render::{MediaResolver, StoreMedia};
use makepad_story_cut::{AssetRef, CutDoc};
use std::path::PathBuf;

fn root(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_motion_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

const DOC: &str = "use mod.motion.*; Motion{size: vec2(640, 360) duration: 3.0 inputs: {headline: \"Hi\"} layers: [Text{text: in(@headline) size: 40 at: vec2(320, 180) align: @center}]}";

#[test]
fn a_published_motion_document_resolves_for_the_renderer() {
    let server_root = root("server");
    let mut config = ServerConfig::new(server_root.clone());
    config.control_addr = "127.0.0.1:0".parse().unwrap();
    config.data_addr = "127.0.0.1:0".parse().unwrap();
    config.bootstrap_admin = true;
    config.log = false;
    let mut server = AssetServer::start(config).expect("server");
    let token = std::fs::read_to_string(server_root.join("admin-token")).expect("token").trim().to_string();
    let mut client_config = ClientConfig::new(root("cache"));
    client_config.token = Some(token);
    let endpoints = ApiEndpoints { control: server.control_addr(), data: server.data_addr() };
    let mut client = AssetClient::connect(client_config, endpoints, Some(server.server_id())).expect("connect");

    // Published the way the Stage host's `write_motion` does.
    let (thumb, tw, th) = thumbnail_jpeg(&vec![0xff20_2020u32; 320 * 180], 320, 180).expect("thumbnail");
    let mut bundle = PublishBundle::new(
        "stage",
        AssetKind::Motion,
        "A title",
        vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, DOC.as_bytes().to_vec(), None)],
        PublishThumbnail::plain(thumb, ThumbnailMedia::Jpeg, tw, th),
        PublishRights::generated_cc0(),
    );
    bundle.tags = vec!["motion".into()];
    let published = client.publish_bundle(&bundle).expect("publish motion");
    let motion = AssetRef { id: published.asset_id.to_string(), rev: published.revision.to_string() };

    let mut media = StoreMedia::new(client);
    assert_eq!(media.motion_source(&motion).expect("motion source"), DOC);

    let edit = format!(
        "use mod.flow.*\nuse mod.cut.*\nlet g = Graphic{{src: asset(\"{}\", \"{}\") span: [0.0, 3.0] params: {{headline: \"From the library\"}}}}\nlet out = CutOutput{{frame: g.out(@frame)}}\nFlow{{\n    kind: @cut\n    output: {{width: 640 height: 360 fps: 30}}\n    duration: 3.0\n    g, out\n}}\n",
        motion.id, motion.rev,
    );
    let doc = CutDoc::load(&edit).unwrap_or_else(|d| panic!("{d:#?}"));
    let media = std::sync::Mutex::new(Box::new(media) as Box<dyn MediaResolver>);
    let assets = resolve_sources(&doc, &media).expect("sources");
    assert_eq!(assets.sources.get(&motion).map(String::as_str), Some(DOC));
    server.shutdown();
}

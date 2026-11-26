//! A finished render filed as the cut's render, over real sockets: a
//! `Video` whose one dependency is the cut revision, under the cut's
//! render alias, with the cut's rights and the poster as its thumbnail.

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig, PublishBundle, PublishBundleFile, PublishRights, PublishThumbnail};
use makepad_asset_data::{AssetKind, AssetRevisionRef, FileRole, MediaType, ThumbnailMedia};
use makepad_asset_store::{AssetServer, ServerConfig};
use makepad_story_edit_render::publish::{publish_edit_render, thumbnail_jpeg};
use makepad_story_edit_render::RenderResult;
use std::path::PathBuf;

fn root(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_render_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_render_is_the_edits_video_and_names_its_revision() {
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

    let poster = vec![0xff30_6090u32; 320 * 180];
    let (thumb, tw, th) = thumbnail_jpeg(&poster, 320, 180).expect("thumbnail");
    let mut edit = PublishBundle::new(
        "gen",
        AssetKind::Cut,
        "A cut",
        vec![PublishBundleFile::bytes(FileRole::Source, MediaType::Text, b"use mod.cut.*\n".to_vec(), None)],
        PublishThumbnail::plain(thumb, ThumbnailMedia::Jpeg, tw, th),
        PublishRights::generated_cc0(),
    );
    edit.media_millis = 4_000;
    let edit = client.publish_bundle(&edit).expect("publish edit");
    let edit_ref = AssetRevisionRef { asset_id: edit.asset_id, revision: edit.revision };

    let mp4 = root("render").with_extension("mp4");
    std::fs::write(&mp4, vec![0x5a; 9_000]).unwrap();
    let (srt, vtt) = (root("render").with_extension("subs.srt"), root("render").with_extension("subs.vtt"));
    std::fs::write(&srt, "1\n00:00:00,300 --> 00:00:01,900\nEvery frame lands exactly\n").unwrap();
    std::fs::write(&vtt, "WEBVTT\n\n1\n00:00:00.300 --> 00:00:01.900\nEvery frame lands exactly\n").unwrap();
    let render = RenderResult {
        path: mp4,
        cut_source: makepad_asset_data::BlobId::hash_of(b"use mod.cut.*\n"),
        sidecars: vec![srt.clone(), vtt.clone()],
        frames: 120,
        size: (320, 180),
        fps: (30, 1),
        duration_ms: 4_000,
        audio: true,
        frame_hashes: vec![1; 120],
        poster: Some((poster, 320, 180)),
    };
    assert_eq!(render.captions().len(), 1);
    assert_eq!(render.captions()[0].node, "subs");
    let published = publish_edit_render(&mut client, edit_ref, None, &render, "A cut (render)").expect("publish render");
    assert!(published.alias.as_ref().is_some_and(|alias| alias.as_str() == format!("gen/render/{}", edit.asset_id)));
    let manifest = client.fetch_asset_manifest(&published.revision).expect("render manifest");
    assert_eq!(manifest.kind, AssetKind::Video);
    assert_eq!(manifest.dependencies, vec![edit_ref]);
    assert_eq!(manifest.rights.license, "CC0-1.0");
    let video = manifest.files.iter().find(|file| file.role == FileRole::Video).expect("video file");
    assert_eq!((video.media, video.byte_len), (MediaType::Mp4, 9_000));
    // The captions travel with it as real files, track 0.
    for (role, marker) in [(FileRole::CaptionsSrt, "00:00:00,300 --> 00:00:01,900"), (FileRole::CaptionsVtt, "WEBVTT")] {
        let file = manifest.files.iter().find(|file| file.role == role).expect("caption file");
        assert_eq!((file.media, file.lod), (MediaType::Text, 0));
        let (blob, len) = (file.blob, file.byte_len);
        let text = String::from_utf8(client.fetch_blob_bytes(&blob, Some(len)).expect("caption bytes")).unwrap();
        assert!(text.contains(marker) && text.contains("Every frame lands exactly"), "{text}");
    }
    assert!(!manifest.files.iter().any(|file| file.role == FileRole::Source), "no overflow document for one track");
    let (render_ref, source) = client.render_of(&edit.asset_id).expect("render_of").expect("a render");
    assert_eq!((render_ref.revision, source), (published.revision, edit_ref));

    // Nine tracks: eight in the caption slots, the ninth in the Source
    // document; none dropped.
    let mut many = render.clone();
    many.sidecars.clear();
    for i in 0..9 {
        for ext in ["srt", "vtt"] {
            let path = root("render").with_extension(format!("t{i}.{ext}"));
            std::fs::write(&path, format!("track {i} {ext}\n")).unwrap();
            many.sidecars.push(path);
        }
    }
    let nine = publish_edit_render(&mut client, edit_ref, None, &many, "Nine tracks").expect("publish nine");
    let manifest = client.fetch_asset_manifest(&nine.revision).expect("nine manifest");
    let lods: Vec<u8> = manifest.files.iter().filter(|file| file.role == FileRole::CaptionsSrt).map(|file| file.lod).collect();
    assert_eq!(lods, (0..8).collect::<Vec<u8>>());
    let overflow = manifest.files.iter().find(|file| file.role == FileRole::Source).expect("overflow document");
    let (blob, len) = (overflow.blob, overflow.byte_len);
    let text = String::from_utf8(client.fetch_blob_bytes(&blob, Some(len)).unwrap()).unwrap();
    assert!(text.contains("\"node\":\"t8\"") && text.contains("track 8 srt"), "{text}");

    // Only a cut revision takes a render this way, and only a render of
    // its own source.
    assert!(publish_edit_render(&mut client, render_ref, None, &render, "no").is_err());
    let other = RenderResult { cut_source: makepad_asset_data::BlobId::hash_of(b"another cut"), ..render.clone() };
    assert!(publish_edit_render(&mut client, edit_ref, None, &other, "no").unwrap_err().contains("another source"));
    server.shutdown();
}

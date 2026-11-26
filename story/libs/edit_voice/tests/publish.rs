//! Takes and transcripts published to a real store over loopback: the
//! store's own checks (labels, roles, dependencies) accept them.

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig};
use makepad_asset_data::{AssetKind, FileRole};
use makepad_asset_store::{AssetServer, ServerConfig};
use makepad_story_edit_voice::lyrics::TranscriptLine;
use makepad_story_edit_voice::publish::{publish_take, publish_transcript};
use makepad_story_edit_voice::*;
use std::path::PathBuf;

fn root(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("edit_voice_{}_{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_take_and_its_transcript_publish_to_a_real_store() {
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

    let voice = VoiceSpec::default();
    let text = "That's it.";
    let pcm = (0..24_000).map(|i| (i as f32 * 0.05).sin() * 0.3).collect::<Vec<_>>();
    let (words, timing) = word_timings(text, &pcm, 24_000, "en", None, None);
    let take = Take { pcm, rate: 24_000, words, timing, key: take_key(&voice, text), phones: Vec::new() };
    let take_ref = publish_take(&mut client, "gen", &take, text, &voice).expect("publish take");
    let manifest = client.fetch_asset_manifest(&take_ref.rev.parse().unwrap()).expect("take manifest");
    assert_eq!(manifest.kind, AssetKind::Audio);
    assert!(manifest.files.iter().any(|file| file.role == FileRole::Lyrics));

    let transcript = Transcript {
        backend: "whisper".into(), model: "ggml-large-v3-turbo.bin".into(), language: "pt-BR".into(), duration_secs: 1.0, timing: "whisper".into(),
        lines: vec![TranscriptLine { start: 0.1, end: 0.9, text: text.into(), words: vec![], confident: false }],
        phones: Vec::new(),
    };
    let transcript_ref = publish_transcript(&mut client, "gen", &transcript, &take_ref, "The take", None).expect("publish transcript");
    let manifest = client.fetch_asset_manifest(&transcript_ref.rev.parse().unwrap()).expect("transcript manifest");
    assert_eq!(manifest.kind, AssetKind::Data);
    assert_eq!(manifest.dependencies.len(), 1);
    server.shutdown();
}

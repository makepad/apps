//! Publish a new head revision of existing store games from edited sources (lane FLEETUP,
//! 2026-10-24: store games still on the deleted HUD verbs, ported to hud_kit parts). It is the
//! Sandbox editor's publish (`run_publish_job`): the head's world set with the main `game.splash`
//! replaced, the head's thumbnail, title and description kept, generated-CC0 rights. Unlike
//! publish_bundled_game it publishes over an edited head. History is kept: the old revisions stay.
//!
//!   publish_game_source <store root> <games root> <slug>...
//!
//! The store is hosted in this process for the publish (loopback planes, no library
//! publisher) through the store's own server. A store another process serves (its `server.lock` is
//! held: a running Sandbox) is published THROUGH that server instead, with the root's `listen`,
//! `server-id` and `admin-token`: the running Sandbox sees the new head live, and store sync carries
//! it on. <games root>/<slug>/game.splash is the new source; a slug whose head already has exactly
//! that source is skipped.
//!
//! NOT a shipped tool: working-tree scaffolding, like repair_games.

use makepad_app_asset_server::{Host, HostConfig};
use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig, PublishRights};
use makepad_asset_importer::games_import;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

fn publish(client: &mut AssetClient, root: &Path, slug: &str) -> Result<String, String> {
    let source = std::fs::read(root.join(slug).join("game.splash")).map_err(|e| format!("source: {e}"))?;
    let (asset, worlds) = games_import::head_world_sources(client, "sandbox", slug)?;
    if worlds.get(games_import::MAIN_SUB) == Some(&source[..]) {
        return Ok("unchanged (head already has this source)".to_string());
    }
    let annotation = client.get_annotation(&asset).map_err(|e| format!("annotation: {e}"))?;
    let thumbnail = games_import::head_thumbnail(client, "sandbox", slug)?;
    games_import::publish_game_revision(
        client,
        "sandbox",
        slug,
        &annotation.title,
        &annotation.description,
        Some(source),
        thumbnail,
        &PublishRights::generated_cc0(),
    )?;
    let (_, after) = games_import::head_world_sources(client, "sandbox", slug)?;
    let blob = after.blob(games_import::MAIN_SUB).map(|b| format!("{b:?}")).unwrap_or_default();
    Ok(format!("published ({asset}, title {:?}, new main source blob {blob})", annotation.title))
}

/// The running server of a store root another process hosts: its `listen` (`ip:control:data`),
/// `server-id` (hex) and root `admin-token`.
fn attached(store: &Path) -> Result<(ApiEndpoints, [u8; 16], String), String> {
    let read = |leaf: &str| {
        std::fs::read_to_string(store.join(leaf)).map(|t| t.trim().to_string()).map_err(|e| format!("{leaf}: {e}"))
    };
    let listen = read("listen")?;
    let mut parts = listen.lines().next().unwrap_or("").split(':');
    let (Some(ip), Some(control), Some(data), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(format!("listen: not ip:control:data: {listen:?}"));
    };
    let ip: std::net::IpAddr = ip.parse().map_err(|_| format!("listen: bad ip {ip:?}"))?;
    let port = |p: &str| p.parse::<u16>().map_err(|_| format!("listen: bad port {p:?}"));
    let endpoints = ApiEndpoints { control: SocketAddr::new(ip, port(control)?), data: SocketAddr::new(ip, port(data)?) };
    let hex = read("server-id")?;
    let mut server_id = [0u8; 16];
    if hex.len() != 32 {
        return Err(format!("server-id: not 32 hex chars: {hex:?}"));
    }
    for (i, byte) in server_id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| format!("server-id: bad hex {hex:?}"))?;
    }
    Ok((endpoints, server_id, read("admin-token")?))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 {
        eprintln!("usage: publish_game_source <store root> <games root> <slug>...");
        std::process::exit(2);
    }
    let store = PathBuf::from(&args[0]);
    let mut config = HostConfig::new(store.clone());
    config.control_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    config.data_addr = SocketAddr::from(([127, 0, 0, 1], 0));
    config.library = None;
    config.work_root = std::env::temp_dir().join("publish-game-source-work");
    let (mut host, endpoints, server_id, token) = match Host::start(&config) {
        Ok(host) => {
            let (endpoints, server_id, token) = (host.endpoints(), host.server_id(), host.token().to_string());
            (Some(host), endpoints, server_id, token)
        }
        Err(host_error) => match attached(&store) {
            Ok((endpoints, server_id, token)) => {
                println!("{} is served by another process: publishing through it at {}", store.display(), endpoints.control);
                (None, endpoints, server_id, token)
            }
            Err(attach_error) => {
                eprintln!("cannot host {}: {host_error}; cannot attach to its server: {attach_error}", store.display());
                std::process::exit(1);
            }
        },
    };
    let mut cfg = ClientConfig::new(std::env::temp_dir().join("publish-game-source-cache"));
    cfg.token = Some(token);
    let mut client = match AssetClient::connect(cfg, endpoints, Some(server_id)) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("connect: {error}");
            if let Some(host) = host.as_mut() {
                host.shutdown();
            }
            std::process::exit(1);
        }
    };
    let mut failed = false;
    for slug in &args[2..] {
        match publish(&mut client, Path::new(&args[1]), slug) {
            Ok(what) => println!("{slug}: {what}"),
            Err(error) => {
                eprintln!("{slug}: FAILED: {error}");
                failed = true;
            }
        }
    }
    drop(client);
    if let Some(host) = host.as_mut() {
        host.shutdown();
    }
    std::process::exit(if failed { 1 } else { 0 });
}

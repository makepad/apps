//! Publish one bundled game into a running store as a new head revision
//! (lane CITYUI, 2026-10-22): the same `publish_game_detailed` the Sandbox's
//! bundled-games sync runs at connect, for a Sandbox that is already up.
//! History is kept; a head the user edited since the last bundled version is
//! left alone (KeptEdited), never overwritten.
//!
//!   publish_bundled_game <store dir> <games root> <slug>
//!
//! <store dir> holds the server's `listen`, `admin-token` and `server-id`
//! (local/asset-library/sandbox/store); <games root> is a checkout's
//! sandbox/resources/games.
//!
//! NOT a shipped tool: working-tree scaffolding, like repair_games.

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig, PublishRights};
use makepad_asset_importer::games_import;
use std::path::Path;

fn connect(dir: &Path) -> Result<AssetClient, String> {
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{name}: {e}"));
    let listen = read("listen")?;
    let parts: Vec<&str> = listen.trim().split(':').collect();
    let [ip, control, data] = parts[..] else { return Err(format!("listen: {listen:?}")) };
    let token = read("admin-token")?;
    let id_hex = read("server-id")?;
    let id_hex = id_hex.trim();
    if id_hex.len() < 32 {
        return Err(format!("server-id: {id_hex:?}"));
    }
    let mut server_id = [0u8; 16];
    for (i, byte) in server_id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&id_hex[i * 2..i * 2 + 2], 16).map_err(|e| format!("server-id: {e}"))?;
    }
    let mut cfg = ClientConfig::new(std::env::temp_dir().join("publish-bundled-game-cache"));
    cfg.token = Some(token.trim().to_string());
    let endpoints = ApiEndpoints {
        control: format!("{ip}:{control}").parse().map_err(|e| format!("control: {e}"))?,
        data: format!("{ip}:{data}").parse().map_err(|e| format!("data: {e}"))?,
    };
    AssetClient::connect(cfg, endpoints, Some(server_id)).map_err(|e| format!("connect: {e}"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [store, root, slug] = &args[..] else {
        eprintln!("usage: publish_bundled_game <store dir> <games root> <slug>");
        std::process::exit(2);
    };
    let games = games_import::scan_games(Path::new(root));
    let Some(game) = games.iter().find(|g| &g.slug == slug) else {
        eprintln!("no bundled game {slug} under {root}");
        std::process::exit(1);
    };
    let result = connect(Path::new(store)).and_then(|mut client| {
        games_import::publish_game_detailed(&mut client, game, "sandbox", &PublishRights::generated_cc0())
    });
    match result {
        Ok(published) => println!("{slug}: {:?} (asset {}, alias {})", published.outcome, published.asset_id, published.alias),
        Err(error) => {
            eprintln!("{slug}: {error}");
            std::process::exit(1);
        }
    }
}

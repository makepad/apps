//! An app's local asset library from the command line: list every live
//! asset of one kind, show what depends on an asset, and delete assets the
//! way the library's Delete button does (`retire_asset`), refusing any that
//! something still uses.
//!
//!   cargo run --release -p makepad-asset-client --example library -- list <kind>
//!   cargo run --release -p makepad-asset-client --example library -- deps <asset_id>...
//!   cargo run --release -p makepad-asset-client --example library -- retire [--apply] <ids-file>
//!
//! It attaches to the store the running app hosts, through the `listen`,
//! `server-id` and `admin-token` files in the app's store root
//! (`paths::app_store_root`; `LIBRARY_APP` names the app, default `stage`).
//! `<kind>` is a catalog kind name (`vjeffect`, `video`, `edit`, ...).
//! `list` prints `asset_id revision alias title`, one asset per line.
//! `retire` reads asset ids (the first word of each line; `#` comments and
//! blank lines skipped) and, with `--apply`, retires each one that has no
//! live dependents; without it, it only says what it would do.

use makepad_asset_client::dto::kind_parse;
use makepad_asset_client::{paths, ApiEndpoints, AssetClient, CatalogQuery, ClientConfig};
use makepad_asset_data::AssetId;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;

fn read(root: &Path, name: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(name))
        .map(|text| text.lines().next().unwrap_or("").trim().to_string())
        .map_err(|e| format!("{}: {e}", root.join(name).display()))
}

fn connect() -> Result<AssetClient, String> {
    let app = std::env::var("LIBRARY_APP").unwrap_or_else(|_| paths::STAGE_APP.to_string());
    let root = paths::app_store_root(&app);
    let listen = read(&root, "listen")?;
    let mut parts = listen.rsplitn(3, ':');
    let (data, control, host) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let ip: IpAddr = host.trim_start_matches('[').trim_end_matches(']').parse().map_err(|_| format!("listen `{listen}`"))?;
    let port = |p: &str| p.parse::<u16>().map_err(|_| format!("listen `{listen}`"));
    let endpoints = ApiEndpoints { control: SocketAddr::new(ip, port(control)?), data: SocketAddr::new(ip, port(data)?) };
    let id_text = read(&root, "server-id")?;
    let mut server_id = [0u8; 16];
    for (i, b) in server_id.iter_mut().enumerate() {
        *b = u8::from_str_radix(id_text.get(i * 2..i * 2 + 2).unwrap_or("zz"), 16).map_err(|_| "server-id".to_string())?;
    }
    let mut config = ClientConfig::new(std::env::temp_dir().join("makepad-library-cli-cache"));
    config.token = Some(read(&root, "admin-token")?);
    AssetClient::connect(config, endpoints, Some(server_id)).map_err(|e| format!("asset store at {} ({}): {e:?}", endpoints.control, root.display()))
}

fn list(client: &AssetClient, kind: &str) -> Result<(), String> {
    let mut query = CatalogQuery::browse(100);
    query.kind = Some(kind_parse(kind).ok_or_else(|| format!("unknown kind `{kind}`"))?);
    query.live_only = true;
    let mut cursor = None;
    let mut count = 0;
    loop {
        let page = client.catalog_search(&query, cursor.as_ref()).map_err(|e| format!("search: {e}"))?;
        for hit in &page.hits {
            let revision = client
                .asset_detail(&hit.asset_id)
                .ok()
                .and_then(|detail| detail.latest_published().map(|head| head.revision.to_string()))
                .unwrap_or_else(|| "-".into());
            let alias = hit.alias.as_ref().map(|a| a.to_string()).unwrap_or_else(|| "-".into());
            println!("{} {revision} {alias} {}", hit.asset_id, hit.title);
            count += 1;
        }
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    eprintln!("{count} live {kind} asset(s)");
    Ok(())
}

/// Every live dependent of `id` as `asset_id kind title`, and how many the
/// caller may not see.
fn dependents(client: &AssetClient, id: &AssetId) -> Result<(Vec<String>, u64), String> {
    let mut after = None;
    let mut found = Vec::new();
    let mut hidden = 0;
    loop {
        let page = client.dependents(id, after.as_ref()).map_err(|e| format!("dependents of {id}: {e}"))?;
        hidden += page.hidden;
        for dep in &page.dependents {
            found.push(format!("{} {:?} {}", dep.asset_id, dep.kind, dep.title));
        }
        match page.dependents.last() {
            Some(last) if (found.len() as u64 + hidden) < page.total => after = Some(last.asset_id),
            _ => break,
        }
    }
    Ok((found, hidden))
}

fn parse_id(text: &str) -> Result<AssetId, String> {
    text.parse().map_err(|_| format!("`{text}` is not an asset id"))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = connect().and_then(|client| match args.first().map(String::as_str) {
        Some("list") => list(&client, args.get(1).map(String::as_str).unwrap_or("vjeffect")),
        Some("deps") => args[1..].iter().try_for_each(|text| {
            let (found, hidden) = dependents(&client, &parse_id(text)?)?;
            println!("{text}: {} dependent(s){}", found.len() as u64 + hidden, if hidden > 0 { format!(" ({hidden} hidden)") } else { String::new() });
            found.iter().for_each(|dep| println!("  {dep}"));
            Ok(())
        }),
        Some("retire") => {
            let apply = args.iter().any(|a| a == "--apply");
            let file = args[1..].iter().find(|a| !a.starts_with("--")).ok_or("retire needs an ids file")?;
            let text = std::fs::read_to_string(file).map_err(|e| format!("{file}: {e}"))?;
            let (mut retired, mut kept) = (0, 0);
            for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
                let id = parse_id(line.split_whitespace().next().unwrap_or(""))?;
                let (found, hidden) = dependents(&client, &id)?;
                if !found.is_empty() || hidden > 0 {
                    kept += 1;
                    println!("KEPT {line} -- used by {}: {}", found.len() as u64 + hidden, found.join("; "));
                } else if !apply {
                    println!("would retire {line}");
                } else {
                    match client.retire_asset(&id) {
                        Ok(dto) => {
                            retired += 1;
                            println!("RETIRED {line} (revisions {}, aliases {}{})", dto.revisions_retired, dto.aliases_dropped, if dto.already_retired { ", already retired" } else { "" });
                        }
                        Err(e) => println!("FAILED {line}: {e}"),
                    }
                }
            }
            eprintln!("{retired} retired, {kept} kept for their dependents{}", if apply { "" } else { " (dry run)" });
            Ok(())
        }
        _ => Err("usage: library list <kind> | deps <asset_id>... | retire [--apply] <ids-file>".into()),
    });
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

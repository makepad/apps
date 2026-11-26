//! Give kit cars their lamps: headlight / taillight sockets and glowing
//! lenses found in each model's own palette ([`vehicle_lamps::annotate`]).
//!
//!   vehicle_lamps dir <folder>
//!       report what each *.glb in the folder would get (nothing written)
//!   vehicle_lamps store <store-root> <aliases-file> [--apply] [--log <file>]
//!       for each alias (one per line) in the running store at <store-root>
//!       (its `listen`, `admin-token`, `server-id`): annotate the head's
//!       render GLB and, with --apply, publish it as a new revision of the
//!       same asset (every other file and the thumbnail reused). Every
//!       alias gets one log line.

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig};
use makepad_asset_client::side_channels::{SideChannelFile, SideChannelOutcome};
use makepad_asset_data::{FileRole, MediaType};
use makepad_asset_importer::vehicle_lamps::{annotate, Lamps};
use std::io::Write;
use std::str::FromStr;

fn describe(lamps: &Lamps) -> String {
    let f = |p: Option<[[f32; 3]; 2]>| p.map_or("-".to_string(), |[l, r]| format!("L({:.2},{:.2},{:.2}) R({:.2},{:.2},{:.2})", l[0], l[1], l[2], r[0], r[1], r[2]));
    format!("head {} tail {} glow {} texels", f(lamps.head), f(lamps.tail), lamps.lit_texels)
}

fn connect(root: &std::path::Path) -> AssetClient {
    let listen = std::fs::read_to_string(root.join("listen")).expect("listen file (is the store running?)");
    let mut parts = listen.trim().split(':');
    let ip = parts.next().expect("ip");
    let control: u16 = parts.next().expect("control").parse().expect("control port");
    let data: u16 = parts.next().expect("data").parse().expect("data port");
    let token = std::fs::read_to_string(root.join("admin-token")).expect("admin token");
    let id_hex = std::fs::read_to_string(root.join("server-id")).expect("server id");
    let id_hex = id_hex.trim();
    let mut server_id = [0u8; 16];
    for (i, b) in server_id.iter_mut().enumerate() {
        *b = u8::from_str_radix(&id_hex[i * 2..i * 2 + 2], 16).expect("hex");
    }
    let mut cfg = ClientConfig::new(std::env::temp_dir().join("vehicle-lamps-cache"));
    cfg.token = Some(token.trim().to_string());
    let endpoints = ApiEndpoints {
        control: format!("{ip}:{control}").parse().expect("control addr"),
        data: format!("{ip}:{data}").parse().expect("data addr"),
    };
    AssetClient::connect(cfg, endpoints, Some(server_id)).expect("connect")
}

fn one(client: &mut AssetClient, alias: &str, apply: bool) -> Result<String, String> {
    let alias = makepad_asset_data::AssetAlias::from_str(alias).map_err(|e| format!("{e}"))?;
    let head = client.resolve_alias(&alias).map_err(|e| format!("resolve: {e}"))?;
    let manifest = client.fetch_asset_manifest(&head.head_revision).map_err(|e| format!("manifest: {e}"))?;
    let file = manifest.files.iter().find(|f| f.role == FileRole::RenderGlb).ok_or("no render GLB")?;
    let glb = client.fetch_blob_bytes(&file.blob, Some(file.byte_len)).map_err(|e| format!("blob: {e}"))?;
    let Some((out, lamps)) = annotate(&glb)? else { return Ok("no lamps found / already annotated".into()) };
    let what = describe(&lamps);
    if !apply {
        return Ok(format!("would write {what}"));
    }
    match client.publish_replaced_files(&head.asset_id, vec![SideChannelFile { role: FileRole::RenderGlb, media: MediaType::Glb, bytes: out }]) {
        Ok(SideChannelOutcome::Published { revision }) => Ok(format!("published {revision} {what}")),
        Ok(SideChannelOutcome::AlreadyPresent { revision }) => Ok(format!("already {revision}")),
        Err(e) => Err(format!("publish: {e}")),
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("dir") => {
            let dir = std::path::PathBuf::from(&args[1]);
            let mut names: Vec<_> = std::fs::read_dir(&dir).expect("dir").filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "glb")).collect();
            names.sort();
            for path in names {
                let glb = std::fs::read(&path).expect("read");
                let name = path.file_name().unwrap().to_string_lossy();
                match annotate(&glb) {
                    Ok(Some((_, lamps))) => println!("{name}: {}", describe(&lamps)),
                    Ok(None) => println!("{name}: -"),
                    Err(e) => println!("{name}: ERROR {e}"),
                }
            }
        }
        Some("store") => {
            let root = std::path::PathBuf::from(&args[1]);
            let aliases = std::fs::read_to_string(&args[2]).expect("aliases file");
            let apply = args.iter().any(|a| a == "--apply");
            let log_path = args.iter().position(|a| a == "--log").and_then(|i| args.get(i + 1));
            let mut log = log_path.map(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).expect("log"));
            let mut client = connect(&root);
            for alias in aliases.lines().map(str::trim).filter(|l| !l.is_empty()) {
                let line = match one(&mut client, alias, apply) {
                    Ok(note) => format!("{alias}\t{note}"),
                    Err(e) => format!("{alias}\tFAILED {e}"),
                };
                println!("{line}");
                if let Some(log) = log.as_mut() { let _ = writeln!(log, "{line}"); }
            }
        }
        _ => eprintln!("usage: vehicle_lamps dir <folder> | store <store-root> <aliases-file> [--apply] [--log <file>]"),
    }
}

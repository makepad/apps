//! Makepad Asset Importer — the headless import/publish tool beside the
//! Asset Server. The generation coordinator that used to be its default
//! mode is gone (aicore: the store stores, the client creates — apps drive
//! the fleet themselves through `makepad-gen`); what remains are the
//! import modes. With `--import-ai-library <dir>` it runs one idempotent
//! import pass over the GENERATED rows of an existing ai-content library
//! (see the lib's `import` module) and exits. With `--watch-ai-library
//! <dir>` it continuously publishes stable new/changed library artifacts
//! until clean shutdown (see the lib's `watch` module). With `--import-pack
//! <dir>` it compiles a licensed local pack into canonical
//! `SourceCollection` / `ImportManifest` bytes plus a local upload plan and
//! exits without contacting the server (see `pack_import.rs`).
//!
//! Credentials never leave this host: the bearer token is read from a
//! local file (typically the server root's `admin-token`, or a scoped
//! publish token).

#[cfg(target_arch = "wasm32")]
fn main() {}

// This CLI drives native files, sockets, and directory watchers; VJ depends on the library only.
#[cfg(not(target_arch = "wasm32"))]
mod native {

// The library importer/watcher now lives in the crate's lib so the Asset UI
// can run the same publication loop in-process against its embedded server.
use makepad_asset_importer::{games_import, import, music_import, pack_import, sound_import, watch};

use makepad_asset_client::{ApiEndpoints, AssetClient, ClientConfig, PublishRights};
use makepad_asset_data::{DerivativePolicy, Redistribution};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

const USAGE: &str = "\
makepad-asset-importer --server <ip:controlport:dataport> --token-file <path> [options]
makepad-asset-importer --import-pack <dir> --out <dir> --source-config <file>
makepad-asset-importer --import-pack <dir> --out <dir> [source/rights flags]
makepad-asset-importer --server <ip:c:d> --token-file <p> --import-music <dir> \
                       [--namespace <ns>]

Modes:
  --import-ai-library <dir>     One idempotent import pass over an
                                ai-content library directory, then exit.
  --watch-ai-library <dir>      Continuously publish stable new/changed
                                library artifacts until SIGINT/SIGTERM.
  --import-games <dir>          Publish every <slug>/game.splash folder
                                under <dir> as a `game` asset (alias
                                <namespace>/games/<slug>; unchanged
                                sources skip, changed ones become a new
                                revision), then exit. Rights default to the
                                named CC0 grant (own-authored sandbox
                                games) unless --license/--redistribution/
                                --derivatives are given.
  --import-music <dir>          Publish every audio file under <dir> as an
                                `audio` asset, one row per track. EVERY
                                relative directory name becomes a catalog
                                tag (plus the constant `music` tag), and
                                ID3/Vorbis title/artist/album name the row
                                when the file carries them. Alias
                                <namespace>/music/<artist>/<title>;
                                unchanged tracks skip, changed bytes become
                                a new revision of the same asset. mp3/ogg/
                                wav are published, flac/m4a/… are listed as
                                unsupported. Rights default to the personal
                                library terms (all rights reserved, LAN
                                local, local-preview derivatives) unless
                                --license/--redistribution/--derivatives
                                are given.
  --import-sounds <dir>         Publish a curated sound library: <dir>/
                                sounds.tsv lists alias, file, title and
                                comma-separated tags per take; each take
                                becomes an `audio` asset under its alias
                                (sfx/footstep/stone-1), tagged, category
                                `sound`. Unchanged takes skip. Rights
                                default to the generated-library grant
                                unless --license/… are given.
  --import-pack <dir>           Compile a licensed local pack (Kenney, …)
                                into canonical SourceCollection and
                                ImportManifest bytes plus a local upload
                                plan, then exit. Does not contact the
                                server. Supported files: png/jpeg/wav/mp4/glb.

Options:
  --server <ip:ctrl:data>       Asset Server endpoints. Or env
                                ASSET_WORKER_SERVER / ASSET_WORKER_SERVERS.
                                Required except --import-pack.
  --token-file <path>           File holding the bearer token (mpat_…).
                                Required (or env ASSET_WORKER_TOKEN) except
                                --import-pack.
  --server-id <32 hex>          Pin the server identity.
  --namespace <ns>              Namespace for imports (default gen;
                                --import-games defaults to sandbox).
  --cache <dir>                 Client cache root (default
                                ~/.makepad-asset-importer). Watch mode owns
                                a separate child.
  --out <dir>                   Output directory for --import-pack.
  --source-config <path>        Source-only JSON identity/rights file for
                                --import-pack (no file list). CLI overrides.
  --source-id <slug>            Approved source collection id.
  --source-title <text>         Source collection title.
  --pack-name <slug>            Pack name (catalog alias segment).
  --pack-version <ver>          Pack version ([a-z0-9._-]).
  --terms-digest <64 hex>       SHA-256 of the immutable terms text.
  --terms-url <url>             URL of the immutable terms document.
  --license-revision <text>     Optional license revision qualifier.
  --source-archive <64 hex>     Optional SHA-256 of the upstream archive.
  --license <id>                Exact license identifier of the published
                                content (e.g. CC0-1.0, CC-BY-4.0). REQUIRED
                                for --import-ai-library/--watch-ai-library/
                                --import-pack: rights are never invented.
  --redistribution <policy>     allowed | attribution-required | forbidden.
                                Required with --license.
  --derivatives <policy>        allowed | attribution-required | forbidden.
                                Required with --license.
  --credits <text>              Required attribution line (mandatory when a
                                policy is attribution-required; required
                                for --import-pack).
  --source <text>               Upstream origin (pack name/URL) of the
                                imported content. Required for --import-pack.
  --quiet                       No stderr logging.
  --help                        This text.
";

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}

fn install_signal_handlers() {
    #[cfg(unix)]
    {
        extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        const SIGINT: i32 = 2;
        const SIGTERM: i32 = 15;
        unsafe {
            signal(SIGINT, on_signal as *const () as usize);
            signal(SIGTERM, on_signal as *const () as usize);
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("makepad-asset-importer: {message}");
    eprintln!("{USAGE}");
    std::process::exit(2);
}

struct Args {
    endpoints: ApiEndpoints,
    server_id: Option<[u8; 16]>,
    token: String,
    import: Option<PathBuf>,
    watch: Option<PathBuf>,
    /// Splash game folders -> `game` assets. Exclusive with every other mode.
    import_games: Option<PathBuf>,
    /// A music directory tree -> `audio` assets. Exclusive with every other
    /// mode.
    import_music: Option<PathBuf>,
    /// A curated sound-library folder (`sounds.tsv` + takes) -> `audio`
    /// assets under the aliases the index names. Exclusive with every other
    /// mode.
    import_sounds: Option<PathBuf>,
    namespace: String,
    cache: PathBuf,
    /// The operator's explicit rights declaration.
    rights: Option<PublishRights>,
    /// Local licensed-pack compiler. Exclusive with every networked mode.
    import_pack: Option<PathBuf>,
    pack_out: Option<PathBuf>,
    source_config: Option<PathBuf>,
    pack_source: pack_import::PackSourceSpec,
    log: bool,
}

fn parse_endpoints(spec: &str) -> Option<ApiEndpoints> {
    let mut parts = spec.split(':');
    let ip: IpAddr = parts.next()?.parse().ok()?;
    let control: u16 = parts.next()?.parse().ok()?;
    let data: u16 = parts.next()?.parse().ok()?;
    Some(ApiEndpoints {
        control: SocketAddr::new(ip, control),
        data: SocketAddr::new(ip, data),
    })
}

fn from_hex16(text: &str) -> Option<[u8; 16]> {
    makepad_asset_client::util::from_hex_exact::<16>(text.trim())
}

fn parse_args() -> Args {
    let raw = std::env::args().skip(1).collect::<Vec<_>>();
    let mut args = raw.into_iter();
    let mut servers: Vec<String> = Vec::new();
    if let Ok(one) = std::env::var("ASSET_WORKER_SERVER") {
        if !one.trim().is_empty() {
            servers.push(one);
        }
    }
    if let Ok(many) = std::env::var("ASSET_WORKER_SERVERS") {
        for part in many.split(',') {
            let spec = part.trim();
            if !spec.is_empty() && !servers.iter().any(|s| s == spec) {
                servers.push(spec.to_string());
            }
        }
    }
    let mut token = std::env::var("ASSET_WORKER_TOKEN").ok();
    let mut token_file: Option<PathBuf> = None;
    let mut server_id = None;
    let mut import = None;
    let mut watch = None;
    let mut import_games = None;
    let mut import_music = None;
    let mut import_sounds = None;
    let mut namespace: Option<String> = None;
    let mut cache: Option<PathBuf> = None;
    let mut license: Option<String> = None;
    let mut redistribution: Option<String> = None;
    let mut derivatives: Option<String> = None;
    let mut credits = String::new();
    let mut source = String::new();
    let mut import_pack = None;
    let mut pack_out = None;
    let mut source_config = None;
    let mut pack_source = pack_import::PackSourceSpec::default();
    let mut log = true;
    let value_of = |name: &str, args: &mut dyn Iterator<Item = String>| -> String {
        match args.next() {
            Some(value) => value,
            None => fail(&format!("{name} needs a value")),
        }
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => {
                let spec = value_of("--server", &mut args);
                if !servers.iter().any(|s| s == &spec) {
                    servers.push(spec);
                }
            }
            "--token-file" => {
                token_file = Some(PathBuf::from(value_of("--token-file", &mut args)))
            }
            "--server-id" => {
                let value = value_of("--server-id", &mut args);
                server_id =
                    Some(from_hex16(&value).unwrap_or_else(|| fail("malformed --server-id")));
            }
            "--import-ai-library" => {
                import = Some(PathBuf::from(value_of("--import-ai-library", &mut args)))
            }
            "--watch-ai-library" => {
                watch = Some(PathBuf::from(value_of("--watch-ai-library", &mut args)))
            }
            "--import-games" => {
                import_games = Some(PathBuf::from(value_of("--import-games", &mut args)))
            }
            "--import-music" => {
                import_music = Some(PathBuf::from(value_of("--import-music", &mut args)))
            }
            "--import-sounds" => {
                import_sounds = Some(PathBuf::from(value_of("--import-sounds", &mut args)))
            }
            "--import-pack" => {
                import_pack = Some(PathBuf::from(value_of("--import-pack", &mut args)))
            }
            "--out" => pack_out = Some(PathBuf::from(value_of("--out", &mut args))),
            "--source-config" => {
                source_config = Some(PathBuf::from(value_of("--source-config", &mut args)))
            }
            "--source-id" => {
                pack_source.source_id = Some(value_of("--source-id", &mut args).trim().to_string())
            }
            "--source-title" => {
                pack_source.source_title =
                    Some(value_of("--source-title", &mut args).trim().to_string())
            }
            "--pack-name" => {
                pack_source.pack_name = Some(value_of("--pack-name", &mut args).trim().to_string())
            }
            "--pack-version" => {
                pack_source.pack_version =
                    Some(value_of("--pack-version", &mut args).trim().to_string())
            }
            "--terms-digest" => {
                pack_source.terms_digest =
                    Some(value_of("--terms-digest", &mut args).trim().to_string())
            }
            "--terms-url" => {
                pack_source.terms_url = Some(value_of("--terms-url", &mut args).trim().to_string())
            }
            "--license-revision" => {
                pack_source.license_revision =
                    Some(value_of("--license-revision", &mut args).trim().to_string())
            }
            "--source-archive" => {
                pack_source.source_archive =
                    Some(value_of("--source-archive", &mut args).trim().to_string())
            }
            "--namespace" => namespace = Some(value_of("--namespace", &mut args)),
            "--cache" => cache = Some(PathBuf::from(value_of("--cache", &mut args))),
            "--license" => license = Some(value_of("--license", &mut args).trim().to_string()),
            "--redistribution" => {
                redistribution = Some(value_of("--redistribution", &mut args).trim().to_string())
            }
            "--derivatives" => {
                derivatives = Some(value_of("--derivatives", &mut args).trim().to_string())
            }
            "--credits" => credits = value_of("--credits", &mut args).trim().to_string(),
            "--source" => source = value_of("--source", &mut args).trim().to_string(),
            "--quiet" => log = false,
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => fail(&format!("unknown flag {other}")),
        }
    }
    if let Some(path) = token_file {
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => token = Some(text.trim().to_string()),
            Ok(_) => fail("token file is empty"),
            Err(error) => fail(&format!("token file {}: {error}", path.display())),
        }
    }
    if license.is_some() {
        pack_source.license = license.clone();
    }
    if redistribution.is_some() {
        pack_source.redistribution = redistribution.clone();
    }
    if derivatives.is_some() {
        pack_source.derivatives = derivatives.clone();
    }
    if !credits.is_empty() {
        pack_source.credits = Some(credits.clone());
    }
    if !source.is_empty() {
        pack_source.source = Some(source.clone());
    }
    if import.is_some() && watch.is_some() {
        fail("--import-ai-library and --watch-ai-library are mutually exclusive");
    }
    if import_games.is_some()
        && (import.is_some()
            || watch.is_some()
            || import_pack.is_some())
    {
        fail("--import-games is exclusive with every other mode");
    }
    if import_music.is_some()
        && (import.is_some()
            || watch.is_some()
            || import_pack.is_some()
            || import_games.is_some())
    {
        fail("--import-music is exclusive with every other mode");
    }
    if import_sounds.is_some()
        && (import.is_some()
            || watch.is_some()
            || import_pack.is_some()
            || import_games.is_some()
            || import_music.is_some())
    {
        fail("--import-sounds is exclusive with every other mode");
    }
    let namespace = namespace.unwrap_or_else(|| {
        if import_games.is_some() {
            "sandbox"
        } else if import_music.is_some() {
            "music"
        } else {
            "gen"
        }
        .to_string()
    });
    if import_pack.is_some() && (import.is_some() || watch.is_some()) {
        fail("--import-pack is exclusive with library import/watch modes");
    }
    if import_pack.is_some() && pack_out.is_none() {
        fail("--out is required with --import-pack");
    }
    if import_pack.is_none()
        && (pack_out.is_some()
            || source_config.is_some()
            || pack_source.has_pack_identity())
    {
        fail("--out / --source-config / pack identity flags require --import-pack");
    }
    let (endpoints, token) = if import_pack.is_some() {
        // Pack compile is local: no server session, no bearer token.
        (
            ApiEndpoints {
                control: SocketAddr::from(([127, 0, 0, 1], 0)),
                data: SocketAddr::from(([127, 0, 0, 1], 0)),
            },
            String::new(),
        )
    } else {
        if servers.is_empty() {
            fail("--server is required");
        }
        let mut parsed = Vec::new();
        for spec in &servers {
            let Some(ep) = parse_endpoints(spec) else {
                fail("malformed --server (want ip:controlport:dataport)")
            };
            parsed.push(ep);
        }
        let endpoints = parsed.remove(0);
        let Some(token) = token else { fail("--token-file is required") };
        (endpoints, token)
    };
    let cache = cache.unwrap_or_else(|| {
        std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir())
            .join(".makepad-asset-importer")
    });
    // Rights are an explicit typed operator input. Import/watch REQUIRE the
    // complete declaration — the library index records no rights and this
    // tool never invents any.
    // --import-pack uses the fuller source contract in pack_import (terms
    // digest/URL included) and must not apply this publish-rights trio, or
    // a source-config plus a lone --license would fail closed incorrectly.
    let rights = if import_pack.is_some() {
        None
    } else {
        match (&license, &redistribution, &derivatives) {
            (None, None, None) => {
                if import.is_some() || watch.is_some() {
                    fail(
                        "--license, --redistribution and --derivatives are required \
                         for library import/watch (rights are never invented)",
                    );
                }
                None
            }
            (Some(license), Some(redistribution), Some(derivatives)) => {
                let declared = PublishRights::declared(
                    license.clone(),
                    credits.clone(),
                    source.clone(),
                    parse_policy_redistribution(redistribution),
                    parse_policy_derivatives(derivatives),
                );
                if (declared.redistribution == Redistribution::AttributionRequired
                    || declared.derivatives == DerivativePolicy::AttributionRequired)
                    && declared.credits.is_empty()
                {
                    fail("attribution-required rights need --credits");
                }
                Some(declared)
            }
            _ => fail("--license, --redistribution and --derivatives must be stated together"),
        }
    };
    Args {
        endpoints,
        server_id,
        token,
        import,
        watch,
        import_games,
        import_music,
        import_sounds,
        namespace,
        cache,
        rights,
        import_pack,
        pack_out,
        source_config,
        pack_source,
        log,
    }
}

fn parse_policy_redistribution(text: &str) -> Redistribution {
    match text {
        "allowed" => Redistribution::Allowed,
        "attribution-required" => Redistribution::AttributionRequired,
        "forbidden" => Redistribution::Forbidden,
        "lan-local" | "user-owned-local" => Redistribution::LanLocal,
        other => fail(&format!("unknown --redistribution policy {other}")),
    }
}

fn parse_policy_derivatives(text: &str) -> DerivativePolicy {
    match text {
        "allowed" => DerivativePolicy::Allowed,
        "attribution-required" => DerivativePolicy::AttributionRequired,
        "forbidden" => DerivativePolicy::Forbidden,
        "local-preview-only" | "local-preview" => DerivativePolicy::LocalPreview,
        other => fail(&format!("unknown --derivatives policy {other}")),
    }
}

pub(super) fn run() {
    let args = parse_args();
    install_signal_handlers();

    // ---- licensed local pack compiler (no server) ----
    if let Some(dir) = args.import_pack {
        let out = args
            .pack_out
            .as_ref()
            .expect("parse_args requires --out for --import-pack");
        match pack_import::compile_pack(
            &dir,
            out,
            args.pack_source,
            args.source_config.as_deref(),
            args.log,
        ) {
            Ok(report) => {
                println!(
                    "import-pack complete: {} assets, {} blobs, source {}, revision {}",
                    report.assets,
                    report.blobs,
                    report.source_digest,
                    report.import_revision
                );
                println!("  {}", report.source_path.display());
                println!("  {}", report.manifest_path.display());
                println!("  {}", report.plan_path.display());
                std::process::exit(0);
            }
            Err(error) => {
                eprintln!("makepad-asset-importer: import-pack failed: {error}");
                std::process::exit(1);
            }
        }
    }

    let cache_leaf = if args.watch.is_some() {
        "watch-cache"
    } else {
        "cache"
    };
    let mut config = ClientConfig::new(args.cache.join(cache_leaf));
    config.token = Some(args.token.clone());
    let mut client = match AssetClient::connect(config, args.endpoints, args.server_id) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("makepad-asset-importer: connect failed: {error}");
            std::process::exit(1);
        }
    };
    if args.log {
        eprintln!(
            "[asset-worker] connected to {} (server {})",
            args.endpoints.control,
            &makepad_asset_client::util::to_hex(&client.server_id())[..8]
        );
    }

    // ---- import mode ----
    if let Some(dir) = args.import {
        let rights = args.rights.expect("parse_args enforces rights for import mode");
        match import::import_library(&mut client, &dir, &args.namespace, &rights, args.log) {
            Ok(report) => {
                println!(
                    "import complete: {} published, {} already present, {} skipped by kind, \
                     {} not generated, {} failed",
                    report.published.len(),
                    report.skipped_existing.len(),
                    report.skipped_kind.len(),
                    report.skipped_scope.len(),
                    report.failed.len()
                );
                for (file, error) in &report.failed {
                    println!("  FAILED {file}: {error}");
                }
                std::process::exit(if report.failed.is_empty() { 0 } else { 1 });
            }
            Err(error) => {
                eprintln!("makepad-asset-importer: import failed: {error}");
                std::process::exit(1);
            }
        }
    }

    // ---- games import mode ----
    if let Some(dir) = args.import_games {
        // Own-authored games carry the named CC0 grant unless the operator
        // declared otherwise; the trio is still honoured when present.
        let rights = args.rights.clone().unwrap_or_else(PublishRights::generated_cc0);
        match games_import::import_games(&mut client, &dir, &args.namespace, &rights, args.log) {
            Ok(report) => {
                println!(
                    "games import complete: {} published, {} updated, {} unchanged, {} kept (edited in the store), {} failed",
                    report.published.len(),
                    report.updated.len(),
                    report.unchanged.len(),
                    report.kept_edited.len(),
                    report.failed.len()
                );
                for slug in &report.kept_edited {
                    println!("  game {slug}: store head edited by user, bundled update not applied");
                }
                for (slug, error) in &report.failed {
                    println!("  FAILED {slug}: {error}");
                }
                std::process::exit(if report.failed.is_empty() { 0 } else { 1 });
            }
            Err(error) => {
                eprintln!("makepad-asset-importer: games import failed: {error}");
                std::process::exit(1);
            }
        }
    }

    // ---- music directory import mode ----
    if let Some(dir) = args.import_music {
        // A personal library states the honest terms unless the operator
        // declared better ones: held locally, servable on this LAN, never
        // redistributed off it.
        let rights = args
            .rights
            .clone()
            .unwrap_or_else(|| music_import::personal_library_rights(&dir));
        let mut progress = |p: music_import::MusicProgress| {
            if args.log && !p.current.is_empty() {
                let stage = match p.stage {
                    music_import::MusicStage::Reading => "reading",
                    music_import::MusicStage::Publishing => "publishing",
                };
                eprintln!("[music-import] {stage} {}/{} {}", p.done, p.total, p.current);
            }
        };
        let cancel = || STOP.load(Ordering::SeqCst);
        match music_import::import_music(
            &mut client,
            &dir,
            &args.namespace,
            &rights,
            args.log,
            &mut progress,
            &cancel,
        ) {
            Ok(report) => {
                println!(
                    "music import complete: {} published, {} updated, {} unchanged, \
                     {} skipped, {} failed{}",
                    report.published.len(),
                    report.updated.len(),
                    report.unchanged.len(),
                    report.skipped.len(),
                    report.failed.len(),
                    if report.cancelled { " (cancelled)" } else { "" }
                );
                for (rel, reason) in &report.skipped {
                    println!("  skipped {rel}: {reason}");
                }
                for (rel, error) in &report.failed {
                    println!("  FAILED {rel}: {error}");
                }
                std::process::exit(if report.failed.is_empty() { 0 } else { 1 });
            }
            Err(error) => {
                eprintln!("makepad-asset-importer: music import failed: {error}");
                std::process::exit(1);
            }
        }
    }

    // ---- sound-library import mode ----
    if let Some(dir) = args.import_sounds {
        let rights = args.rights.clone().unwrap_or_else(sound_import::generated_rights);
        match sound_import::import_sounds(&mut client, &dir, &rights, args.log) {
            Ok(report) => {
                println!(
                    "sound import complete: {} published, {} updated, {} unchanged, {} failed",
                    report.published.len(),
                    report.updated.len(),
                    report.unchanged.len(),
                    report.failed.len()
                );
                for (alias, error) in &report.failed {
                    println!("  FAILED {alias}: {error}");
                }
                std::process::exit(if report.failed.is_empty() { 0 } else { 1 });
            }
            Err(error) => {
                eprintln!("makepad-asset-importer: sound import failed: {error}");
                std::process::exit(1);
            }
        }
    }

    // ---- continuous import mode ----
    if let Some(dir) = args.watch {
        if args.log {
            eprintln!("[asset-worker] watching {}", dir.display());
        }
        let rights = args.rights.expect("parse_args enforces rights for watch mode");
        watch::run(
            &mut client,
            &dir,
            &args.namespace,
            &rights,
            args.log,
            &STOP,
            watch::Timing::default(),
            |_| {},
        );
        if args.log {
            eprintln!("[asset-worker] watch stopped");
        }
        return;
    }

    // No mode flag matched. The generation coordinator that used to run
    // here is gone (aicore: apps drive the fleet themselves through
    // makepad-gen); this tool only imports.
    fail("pick a mode: --import-ai-library / --watch-ai-library / --import-games / --import-music / --import-sounds / --import-pack");
}

}

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    native::run();
}

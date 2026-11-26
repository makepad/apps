//! `makepad-asset-server` — run the Asset Server as its own process.
//!
//! Parse flags into a [`HostConfig`], start the host, and wait for
//! SIGINT/SIGTERM to shut it down cleanly. It serves ONE app's store, named
//! explicitly (`--app stage`, `--app sandbox`, or `--root <dir>`), on
//! ephemeral loopback ports, and publishes that app's library. The LAN
//! reaches it only through store sync (`--sync <file>`: the fleet TLS front). Apps host their own store in-process by default; this binary is
//! the tool for running one out of process.
//!
//! See `README.md` beside this file for the deployment runbook.

use makepad_app_asset_server::{Host, HostConfig};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const USAGE: &str = "\
makepad-asset-server --app <name> [options]
makepad-asset-server --root <dir> [options]

The standalone, multiplayer-first Asset Server: catalog + CAS, events hub,
game rooms, store sync, plus the ai-content library publisher. Clients
(asset-ui, vj, sandbox) attach to it and may come and go without ever
taking the store down. Generation runs in the creating apps over their own
fleet connections — the store stores.

Options:
  --app <name>            Serve this app's store (one library per app):
                          stage = <checkout>/local/asset-library/store
                          (or $STAGE_ASSET_ROOT / $AI_CONTENT_ASSET_ROOT),
                          any other = <checkout>/local/asset-library/<name>/store
                          (or $<NAME>_ASSET_ROOT)
  --root <dir>            Serve this store root instead. One of --app and
                          --root is required.
  --control <addr>        Control plane bind, loopback only (default
                          127.0.0.1:0 — ephemeral; the real address is
                          written to <root>/listen)
  --data <addr>           Data plane bind, loopback only (default 127.0.0.1:0)
  --sync <file>           Sync this store with the fleet (a store-sync file:
                          listen <LAN ip:port>, peer <ip:port>...; the fleet
                          TLS front, fleet machines only). Default with
                          --app: that app's store-sync file, when it exists
  --library <dir>         ai-content library to publish continuously.
                          Default with --app: that app's library
                          (stage: $MAKEPAD_ASSET_LIBRARY, else
                          <checkout>/local/asset-library; others: its
                          <name> child). With only --root: none.
  --no-library            Do not run the library publisher
  --max-conns <n>         Live connections per plane (default 64, max 1024);
                          a host serving many app instances needs more
  --max-blob-bytes <n>    Largest blob the store admits. Default with --app:
                          that app's setting (stage: 16 GiB); otherwise the
                          store default (256 MiB)
  --namespace <ns>        Namespace for published rows (default gen)
  --work <dir>            Parent for the loops' client caches
                          (default: the server root's parent)
  --quiet                 No stderr logging
  --help                  This text
";

static STOP: AtomicBool = AtomicBool::new(false);

// Minimal async-signal-safe handling: the handler only flips an atomic and
// the main thread polls it.
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
    eprintln!("makepad-asset-server: {message}");
    eprintln!("{USAGE}");
    std::process::exit(2);
}

fn parse_config() -> HostConfig {
    let mut args = std::env::args().skip(1);
    let mut root: Option<PathBuf> = None;
    let mut app: Option<String> = None;
    let mut max_blob_bytes: Option<u64> = None;
    let mut max_conns: Option<usize> = None;
    let mut control: Option<SocketAddr> = None;
    let mut data: Option<SocketAddr> = None;
    let mut sync: Option<PathBuf> = None;
    let mut library: Option<PathBuf> = None;
    let mut no_library = false;
    let mut namespace: Option<String> = None;
    let mut work: Option<PathBuf> = None;
    let mut log = true;

    let value_of = |name: &str, args: &mut dyn Iterator<Item = String>| -> String {
        match args.next() {
            Some(value) => value,
            None => fail(&format!("{name} needs a value")),
        }
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = Some(PathBuf::from(value_of("--root", &mut args))),
            "--app" => {
                let name = value_of("--app", &mut args);
                let valid = !name.is_empty()
                    && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
                if !valid {
                    fail("--app wants a lower-case name ([a-z0-9_-])");
                }
                app = Some(name);
            }
            "--control" => {
                let value = value_of("--control", &mut args);
                control = Some(value.parse().unwrap_or_else(|_| fail("malformed --control address")));
            }
            "--data" => {
                let value = value_of("--data", &mut args);
                data = Some(value.parse().unwrap_or_else(|_| fail("malformed --data address")));
            }
            "--sync" => sync = Some(PathBuf::from(value_of("--sync", &mut args))),
            "--library" => library = Some(PathBuf::from(value_of("--library", &mut args))),
            "--no-library" => no_library = true,
            "--max-blob-bytes" => {
                let value = value_of("--max-blob-bytes", &mut args);
                let n: u64 = value.parse().unwrap_or_else(|_| fail("malformed --max-blob-bytes"));
                if n == 0 {
                    fail("--max-blob-bytes must be positive");
                }
                max_blob_bytes = Some(n);
            }
            "--namespace" => namespace = Some(value_of("--namespace", &mut args)),
            "--max-conns" => {
                let value = value_of("--max-conns", &mut args);
                let n: usize = value.parse().unwrap_or_else(|_| fail("malformed --max-conns"));
                if n == 0 || n > 1024 {
                    fail("--max-conns must be 1..=1024");
                }
                max_conns = Some(n);
            }
            "--work" => work = Some(PathBuf::from(value_of("--work", &mut args))),
            "--quiet" => log = false,
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => fail(&format!("unknown flag {other}")),
        }
    }
    if no_library && library.is_some() {
        fail("--library and --no-library contradict each other");
    }

    use makepad_asset_client::paths;
    let root = match (root, &app) {
        (Some(root), _) => root,
        (None, Some(app)) => paths::app_store_root(app),
        (None, None) => fail("name the store: --app <name> or --root <dir>"),
    };
    let mut config = HostConfig::new(root);
    config.max_conns = max_conns;
    config.max_blob_bytes = max_blob_bytes.or_else(|| app.as_deref().and_then(paths::app_max_blob_bytes));
    if let Some(addr) = control {
        config.control_addr = addr;
    }
    if let Some(addr) = data {
        config.data_addr = addr;
    }
    use makepad_app_asset_server::sync_config;
    config.sync = match (&sync, &app) {
        (Some(path), _) => {
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| fail(&format!("{}: {e}", path.display())));
            let (cfg, skipped) = sync_config::parse(&text, path).unwrap_or_else(|e| fail(&e));
            skipped.iter().for_each(|s| eprintln!("config error: {s}"));
            Some(cfg)
        }
        (None, Some(app)) => sync_config::load(&app.to_ascii_uppercase(), app).unwrap_or_else(|e| fail(&e)),
        (None, None) => None,
    };
    config.library = if no_library {
        None
    } else {
        library.or_else(|| app.as_deref().map(paths::app_library_root))
    };
    if let Some(namespace) = namespace {
        if namespace.is_empty() {
            fail("--namespace needs a value");
        }
        config.namespace = namespace;
    }
    if let Some(work) = work {
        config.work_root = work;
    }
    config.log = log;
    config
}

fn main() {
    let config = parse_config();
    install_signal_handlers();
    if config.log {
        eprintln!(
            "[asset-host] root {} · library {} · sync {}",
            config.root.display(),
            config
                .library
                .as_ref()
                .map(|dir| dir.display().to_string())
                .unwrap_or_else(|| "off".to_string()),
            config.sync.as_ref().map(|s| s.listen.to_string()).unwrap_or_else(|| "off".to_string()),
        );
    }
    let mut host = match Host::start(&config) {
        Ok(host) => host,
        Err(error) => {
            eprintln!("makepad-asset-server: failed to start: {error}");
            std::process::exit(1);
        }
    };
    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(200));
    }
    host.shutdown();
}

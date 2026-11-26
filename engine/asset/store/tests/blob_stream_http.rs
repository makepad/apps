//! Blob downloads stream from disk off the state thread: a multi-gigabyte
//! blob is served in bounded chunks (full body or `Range`, so a video reader
//! can seek), other clients keep being answered while it transfers, and a
//! corrupt object never reaches a client complete.
//!
//! The large blob is a SPARSE file admitted by reference: gigabytes of
//! logical size, a few bytes of real data on disk.

use makepad_asset_client::{Api, ApiEndpoints, AssetClient, ClientConfig, HttpLimits};
use makepad_asset_data::sha256::Sha256;
use makepad_asset_store::{AssetServer, AssetServerCore, Budgets, ServerConfig};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

static DIR_COUNTER: AtomicU64 = AtomicU64::new(0);
const NOW: u64 = 1_700_000_000_000;
const MAX_BLOB: u64 = 8 << 30;

fn test_root(name: &str) -> PathBuf {
    let n = DIR_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("mp_asset_stream_{}_{}_{}", std::process::id(), n, name))
}

fn budgets() -> Budgets {
    let mut b = Budgets::default_v1();
    b.max_blob_bytes = MAX_BLOB;
    b
}

fn start_server(root: &Path) -> (AssetServer, String) {
    let mut cfg = ServerConfig::new(root.to_path_buf());
    cfg.control_addr = "127.0.0.1:0".parse().unwrap();
    cfg.data_addr = "127.0.0.1:0".parse().unwrap();
    cfg.bootstrap_admin = true;
    cfg.log = false;
    cfg.gc_janitor_steps = 0;
    cfg.budgets = budgets();
    let server = AssetServer::start(cfg).expect("server start");
    let token = std::fs::read_to_string(root.join("admin-token")).unwrap().trim().to_string();
    (server, token)
}

fn endpoints(server: &AssetServer) -> ApiEndpoints {
    ApiEndpoints { control: server.control_addr(), data: server.data_addr() }
}

fn connect(server: &AssetServer, token: &str, cache: &str) -> AssetClient {
    let mut cfg = ClientConfig::new(test_root(cache));
    cfg.token = Some(token.to_string());
    AssetClient::connect(cfg, endpoints(server), Some(server.server_id())).expect("connect")
}

/// A sparse file of `len` logical bytes with a few markers written into it.
fn sparse_file(path: &Path, len: u64, markers: &[(u64, &[u8])]) {
    let mut f = std::fs::File::create(path).unwrap();
    f.set_len(len).unwrap();
    for (at, bytes) in markers {
        f.seek(SeekFrom::Start(*at)).unwrap();
        f.write_all(bytes).unwrap();
    }
    f.sync_all().unwrap();
}

#[test]
fn a_multi_gigabyte_blob_streams_seeks_and_never_stalls_other_clients() {
    let root = test_root("big");
    let src = test_root("big_src");
    std::fs::create_dir_all(&src).unwrap();
    let size: u64 = (2 << 30) + (3 << 20) + 5; // past 2^31, not chunk-aligned
    let mid = (1 << 30) + 7;
    let path = src.join("long-render.mp4");
    sparse_file(&path, size, &[(0, b"HEAD"), (mid, b"MIDDLE"), (size - 4, b"TAIL")]);
    let blob = {
        let core = AssetServerCore::open(&root, budgets()).unwrap();
        let commit = core.put_blob_ref(&path, NOW).unwrap();
        assert_eq!(commit.size, size);
        commit.blob_id
    };
    let (mut server, token) = start_server(&root);
    let ends = endpoints(&server);
    let api = Api::new(ends, HttpLimits::default_v1(), Some(token.clone())).unwrap();

    // ---- a full download of a not-yet-verified blob, while others ask ----
    let done = Arc::new(AtomicBool::new(false));
    let first_byte = Arc::new(AtomicU64::new(u64::MAX));
    let downloader = {
        let (token, done, first_byte) = (token.clone(), done.clone(), first_byte.clone());
        std::thread::spawn(move || {
            let api = Api::new(ends, HttpLimits::default_v1(), Some(token)).unwrap();
            let started = Instant::now();
            let mut resp = api.blob_get(&blob, None, 600_000).unwrap();
            assert_eq!(resp.head().content_length, size);
            let mut hasher = Sha256::new();
            let mut buf = vec![0u8; 1 << 20];
            let mut total = 0u64;
            loop {
                let n = resp.read_chunk(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                if total == 0 {
                    first_byte.store(started.elapsed().as_millis() as u64, Ordering::SeqCst);
                }
                hasher.update(&buf[..n]);
                total += n as u64;
            }
            done.store(true, Ordering::SeqCst);
            (total, hasher.finalize())
        })
    };
    // Every other client keeps being answered by the (single) state thread
    // while gigabytes stream out on the data plane.
    let other = connect(&server, &token, "big_other");
    let mut calls = 0u32;
    let mut slowest = Duration::ZERO;
    while !done.load(Ordering::SeqCst) {
        let t = Instant::now();
        other.assets_page(None, None, 10).expect("listing during a large download");
        slowest = slowest.max(t.elapsed());
        calls += 1;
        std::thread::sleep(Duration::from_millis(5));
    }
    let (total, digest) = downloader.join().unwrap();
    assert_eq!(total, size);
    assert_eq!(&digest, blob.as_bytes());
    assert!(calls >= 5, "the download was too quick to overlap anything ({calls} calls)");
    assert!(slowest < Duration::from_millis(500), "a listing waited {slowest:?} behind the download");
    let ttfb = first_byte.load(Ordering::SeqCst);
    assert!(ttfb < 1_000, "first byte after {ttfb} ms: the body was not streamed");

    // ---- seeks: closed ranges straight into a caller buffer ----
    let client = connect(&server, &token, "big_seek");
    let mut buf = [0u8; 6];
    assert_eq!(client.read_blob_range(&blob, mid, &mut buf).unwrap(), 6);
    assert_eq!(&buf, b"MIDDLE");
    let mut tail = [0u8; 16];
    assert_eq!(client.read_blob_range(&blob, size - 4, &mut tail).unwrap(), 4);
    assert_eq!(&tail[..4], b"TAIL");
    assert_eq!(api.blob_range(&blob, 0, &mut [0u8; 4], 10_000).unwrap(), 4);
    assert!(client.read_blob_range(&blob, size, &mut buf).is_err());
    // A verified blob's ranges are immediate.
    let t = Instant::now();
    for i in 0..20u64 {
        client.read_blob_range(&blob, i * (size / 20), &mut buf).unwrap();
    }
    assert!(t.elapsed() < Duration::from_secs(2), "20 seeks took {:?}", t.elapsed());
    server.shutdown();
}

#[test]
fn a_corrupt_object_never_reaches_a_client_complete() {
    let root = test_root("corrupt");
    let (mut server, token) = start_server(&root);
    let mut client = connect(&server, &token, "corrupt_cache");
    let api = Api::new(endpoints(&server), HttpLimits::default_v1(), Some(token.clone())).unwrap();
    let bytes: Vec<u8> = (0..5_000_000u32).map(|i| (i * 7 + 3) as u8).collect();
    let blob = api.upload_blob("gen", &bytes).unwrap();

    // Served verified: full body and a range, both exact.
    let mut part = [0u8; 10];
    assert_eq!(client.read_blob_range(&blob, 4_000_000, &mut part).unwrap(), 10);
    assert_eq!(&part[..], &bytes[4_000_000..4_000_010]);
    assert_eq!(client.fetch_blob_bytes(&blob, Some(bytes.len() as u64)).unwrap(), bytes);

    // Flip one byte of the stored object in place (same length).
    let hex = blob.to_string();
    let hex = hex.strip_prefix("sha256:").unwrap();
    let object = root.join("cas").join("objects").join(&hex[..2]).join(&hex[2..4]).join(hex);
    std::thread::sleep(Duration::from_millis(20));
    let mut corrupt = std::fs::read(&object).unwrap();
    corrupt[4_999_000] ^= 0xFF;
    std::fs::write(&object, &corrupt).unwrap();

    // A range re-verifies the changed file and refuses; a full body is cut
    // short before its last chunk, so no client ever holds it complete.
    assert!(client.read_blob_range(&blob, 0, &mut part).is_err());
    let mut resp = api.blob_get(&blob, None, 30_000).unwrap();
    let mut buf = vec![0u8; 1 << 20];
    let mut got = 0u64;
    let outcome = loop {
        match resp.read_chunk(&mut buf) {
            Ok(0) => break Ok(()),
            Ok(n) => got += n as u64,
            Err(e) => break Err(e),
        }
    };
    assert!(outcome.is_err(), "a corrupt body arrived complete");
    assert!(got < bytes.len() as u64);
    client.forget_blob(&blob);
    assert!(!matches!(client.fetch_blob_bytes(&blob, Some(bytes.len() as u64)), Ok(_)));
    server.shutdown();
}

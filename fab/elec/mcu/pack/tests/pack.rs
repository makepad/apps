use fab_elec_mcu_pack::{
    espressif_s3, fetch, install, verify, CancelToken, Error, Manifest, PackFile, Progress,
    Repository,
};
use makepad_core_util::sha256::sha256_hex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

const CORE: &str = "wifi/chip/core.a";

#[derive(Clone, Copy)]
enum Reply {
    Good,
    Tampered,
    Oversize,
    Redirect,
}

struct Fixture {
    root: PathBuf,
    staging: PathBuf,
    dest: PathBuf,
    manifest: Manifest,
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<String>>>,
    server: Option<JoinHandle<()>>,
}

impl Fixture {
    fn new(reply: Reply) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = loop {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("fab-mcu-pack-{}-{n}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => break path,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create fixture directory: {e}"),
            }
        };
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture to loopback");
        let addr = listener.local_addr().unwrap();
        let mut bodies = HashMap::new();
        let mut files = Vec::new();
        for (repo, path, size) in [
            ("wifi", "LICENSE", 151),
            ("wifi", "chip/core.a", 9301),
            ("phy", "chip/core.a", 503),
        ] {
            let bytes: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
            files.push(PackFile {
                repository: repo.into(),
                path: path.into(),
                size,
                sha256: sha256_hex(&bytes),
            });
            bodies.insert(format!("{repo}/{path}"), bytes);
        }
        let manifest = Manifest {
            id: "fixture-pack".into(),
            chip: "fixture-chip".into(),
            license: "test license".into(),
            license_sha256: files[0].sha256.clone(),
            import_map_version: "fixture-v1".into(),
            repositories: ["wifi", "phy"]
                .iter()
                .map(|name| Repository {
                    name: name.to_string(),
                    commit: format!("{name}-commit"),
                    url_base: format!("http://{addr}/pinned/{name}/"),
                })
                .collect(),
            files,
        };
        let requests = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&requests);
        let server = thread::spawn(move || {
            for incoming in listener.incoming() {
                let mut stream = incoming.expect("accept fixture connection");
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    break; // Fixture drop wakes accept with an empty connection.
                }
                let Some((target, _)) = line.strip_prefix("GET ").and_then(|s| s.split_once(' '))
                else {
                    let _ = stream
                        .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n");
                    continue;
                };
                let target = target.to_owned();
                loop {
                    line.clear();
                    if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                        break;
                    }
                }
                log.lock().unwrap().push(target.clone());
                let name = target
                    .strip_prefix("/mirror/")
                    .or_else(|| target.strip_prefix("/pinned/"));
                let Some(body) = name.and_then(|key| bodies.get(key)) else {
                    let _ =
                        stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
                    continue;
                };
                let mut body = body.clone();
                let mut head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                if name == Some(CORE) {
                    match reply {
                        Reply::Good => {}
                        Reply::Tampered => body[20] ^= 1,
                        Reply::Oversize => {
                            // Exercise the streaming limit without a declared Content-Length.
                            head = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".into();
                            body.push(1);
                        }
                        Reply::Redirect => {
                            head = format!("HTTP/1.1 302 Found\r\nLocation: http://{addr}/second-hop\r\nContent-Length: 0\r\n\r\n");
                            body.clear();
                        }
                    }
                }
                let mut response = head.into_bytes();
                response.extend(body);
                let _ = stream.write_all(&response); // A cancelled client may already have closed.
            }
        });
        Self {
            staging: root.join("staging"),
            dest: root.join("installed.pack"),
            root,
            manifest,
            addr,
            requests,
            server: Some(server),
        }
    }

    fn fetch(&self, cancel: &CancelToken, progress: impl FnMut(Progress)) -> Result<(), Error> {
        let base = format!("http://{}/mirror/", self.addr);
        fetch(&self.manifest, Some(&base), &self.staging, cancel, progress)
    }

    fn get(&self) -> Result<(), Error> {
        self.fetch(&CancelToken::new(), |_| {})
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(TcpStream::connect(self.addr).expect("wake fixture listener"));
        self.server
            .take()
            .unwrap()
            .join()
            .expect("fixture server finished");
        fs::remove_dir_all(&self.root).expect("remove fixture directory");
    }
}

#[test]
fn fetch_resume_and_install() {
    let f = Fixture::new(Reply::Good);
    let mut events = Vec::new();
    f.fetch(&CancelToken::new(), |p| events.push(p))
        .expect("fetch fixture");
    let total = f.manifest.total_bytes();
    assert!(
        events
            .iter()
            .all(|p| p.total_bytes == total && p.bytes_done <= total),
        "progress uses pack totals"
    );
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].bytes_done <= pair[1].bytes_done),
        "progress never decreases between files"
    );
    assert_eq!(
        events.last().unwrap().bytes_done,
        total,
        "progress accounts for every downloaded byte"
    );
    let routes: Vec<_> = f
        .manifest
        .files
        .iter()
        .map(|file| format!("/mirror/{}/{}", file.repository, file.path))
        .collect();
    assert_eq!(
        f.requests(),
        routes,
        "one GET at the override base for each repository-relative path"
    );

    events.clear();
    f.fetch(&CancelToken::new(), |p| events.push(p))
        .expect("reuse staged files");
    assert_eq!(
        f.requests(),
        routes,
        "verified files trigger no additional requests"
    );
    assert_eq!(
        events.last().unwrap().bytes_done,
        total,
        "verified files count toward progress"
    );
    fs::write(f.staging.join(CORE), vec![0; 9301]).unwrap();
    let err = verify(&f.manifest, &f.staging)
        .expect_err("same-size staged damage must fail verification");
    assert!(
        err.to_string().contains("core.a") && err.to_string().contains("hash"),
        "verification names the file with the wrong hash: {err}"
    );
    fs::write(
        f.staging.join(format!("{CORE}.part")),
        b"interrupted download",
    )
    .unwrap();
    f.get().expect("repair damaged staged file");
    let mut repaired = routes;
    repaired.push(format!("/mirror/{CORE}"));
    assert_eq!(
        f.requests(),
        repaired,
        "only the changed file is fetched again"
    );

    let installed = install(&f.manifest, &f.staging, &f.dest).expect("install fixture");
    assert_eq!(installed, f.dest, "install returns the destination");
    verify(&f.manifest, &installed).expect("all installed sizes and hashes match");
    assert!(!f.staging.exists(), "install moves staging");
    let provenance =
        fs::read_to_string(installed.join("PROVENANCE.txt")).expect("installed provenance");
    assert!(
        provenance.contains(&f.manifest.id) && provenance.contains(&f.manifest.license),
        "provenance identifies the pack and license"
    );
    for repo in &f.manifest.repositories {
        assert!(
            provenance.contains(&format!("{} {} {}", repo.name, repo.commit, repo.url_base)),
            "provenance pins repository {}",
            repo.name
        );
    }
    for file in &f.manifest.files {
        let record = format!(
            "{}/{} {} bytes SHA-256 {}",
            file.repository, file.path, file.size, file.sha256
        );
        assert!(provenance.contains(&record), "provenance lists {record}");
    }
    assert_eq!(
        install(&f.manifest, &f.staging, &f.dest).expect("reuse verified destination"),
        installed,
        "install is idempotent after staging was moved"
    );
}

#[test]
fn tampered_byte_is_never_published() {
    let f = Fixture::new(Reply::Tampered);
    fs::create_dir(&f.dest).unwrap();
    fs::write(f.dest.join("existing"), b"keep").unwrap();
    let err = f.get().expect_err("tampered file must fail");
    assert!(
        matches!(err, Error::Hash(_)),
        "same-size corruption fails the hash check: {err}"
    );
    assert!(
        err.to_string().contains("core.a"),
        "hash error names the damaged file: {err}"
    );
    assert!(
        !f.staging.join(CORE).exists(),
        "no final file for a failed hash"
    );
    assert!(
        !f.staging.join(format!("{CORE}.part")).exists(),
        "failed temporary file is removed"
    );
    assert_eq!(
        fs::read(f.dest.join("existing")).unwrap(),
        b"keep",
        "old destination is intact"
    );
}

#[test]
fn refuses_stream_larger_than_manifest() {
    let f = Fixture::new(Reply::Oversize);
    let err = f.get().expect_err("oversize response must fail");
    assert!(
        matches!(
            err,
            Error::Http {
                source: makepad_network::blocking_http::Error::ResponseTooLarge,
                ..
            }
        ),
        "the HTTP reader enforces the limit: {err}"
    );
    assert!(
        !f.staging.join(CORE).exists(),
        "oversize file is never published"
    );
}

#[test]
fn redirects_end_at_first_hop() {
    let f = Fixture::new(Reply::Redirect);
    let err = f.get().expect_err("redirect must fail");
    assert!(
        matches!(err, Error::Status { status: 302, .. }),
        "redirect status is reported: {err}"
    );
    assert_eq!(
        f.requests(),
        ["/mirror/wifi/LICENSE", "/mirror/wifi/chip/core.a"],
        "redirect target is never contacted"
    );
    assert!(!f.staging.join(CORE).exists(), "redirect publishes no file");
}

#[test]
fn cancel_from_progress_during_a_file() {
    let f = Fixture::new(Reply::Good);
    let cancel = CancelToken::new();
    let start = f.manifest.files[0].size;
    let end = start + f.manifest.files[1].size;
    let err = f
        .fetch(&cancel, |p| {
            if p.file == CORE && (start < p.bytes_done && p.bytes_done < end) {
                cancel.cancel();
            }
        })
        .expect_err("mid-file cancellation must stop the download");
    assert!(
        matches!(err, Error::Cancelled),
        "fetch reports cancellation: {err}"
    );
    assert!(
        !f.dest.exists(),
        "cancellation leaves the destination absent"
    );
    assert!(
        !f.staging.join(CORE).exists(),
        "cancelled file stays unpublished"
    );
    assert!(
        f.staging.join("wifi/LICENSE").is_file(),
        "earlier verified file survives cancellation"
    );
    f.get().expect("retry a cancelled download");
    assert_eq!(
        f.requests()
            .iter()
            .filter(|p| *p == "/mirror/wifi/LICENSE")
            .count(),
        1,
        "retry keeps the earlier file"
    );
}

#[test]
fn install_replaces_damaged_destination_only_after_verification() {
    let f = Fixture::new(Reply::Good);
    fetch(&f.manifest, None, &f.staging, &CancelToken::new(), |_| {})
        .expect("fetch from original loopback bases");
    let bad = f.root.join("installed.pack.bad");
    fs::create_dir_all(f.dest.join("wifi")).unwrap();
    fs::write(f.dest.join("wifi/LICENSE"), b"damaged").unwrap();
    fs::create_dir(&bad).unwrap();
    fs::write(bad.join("obsolete"), b"old backup").unwrap();

    fs::remove_file(f.staging.join(CORE)).unwrap();
    let err = install(&f.manifest, &f.staging, &f.dest).expect_err("incomplete staging must fail");
    assert!(
        err.to_string().contains("missing") && err.to_string().contains("core.a"),
        "verification names the missing file: {err}"
    );
    assert!(
        bad.join("obsolete").exists() && f.dest.join("wifi/LICENSE").exists(),
        "failed install preserves destination and backup"
    );
    let err = verify(&f.manifest, &f.dest).expect_err("short license must fail");
    assert!(
        err.to_string().contains("size") && err.to_string().contains("LICENSE"),
        "verification names the file with the wrong size: {err}"
    );

    f.get().expect("complete staging");
    install(&f.manifest, &f.staging, &f.dest).expect("replace damaged destination");
    verify(&f.manifest, &f.dest).expect("replacement verifies");
    assert_eq!(
        fs::read(bad.join("wifi/LICENSE")).unwrap(),
        b"damaged",
        "the displaced destination is retained under .bad"
    );
    assert!(!bad.join("obsolete").exists(), "older backup is replaced");
    assert!(!f.staging.exists(), "replacement consumes staging");
}

#[test]
fn espressif_manifest_is_consistent() {
    let pack = espressif_s3();
    assert_eq!(pack.files.len(), 11, "all pinned files are present");
    let mut names = HashSet::new();
    for file in &pack.files {
        assert!(
            names.insert((&file.repository, &file.path)),
            "duplicate file {}/{}",
            file.repository,
            file.path
        );
    }
    assert_eq!(
        pack.repositories.len(),
        2,
        "both upstream repositories are pinned"
    );
    for repo in &pack.repositories {
        assert_eq!(
            repo.url_base,
            format!(
                "https://raw.githubusercontent.com/espressif/{}/{}/",
                repo.name, repo.commit
            ),
            "URL pins the commit for {}",
            repo.name
        );
        let license = pack
            .files
            .iter()
            .find(|f| f.repository == repo.name && f.path == "LICENSE")
            .expect("each repository has a LICENSE");
        assert_eq!(
            license.sha256, pack.license_sha256,
            "LICENSE hash matches for {}",
            repo.name
        );
    }
    assert_eq!(
        pack.total_bytes(),
        pack.files.iter().map(|f| f.size).sum::<u64>(),
        "total is the sum of all file sizes"
    );
    assert_eq!(pack.total_bytes(), 3_820_124, "pinned pack byte count");
    let key = pack.key();
    assert_eq!(
        key, "40cfbd824989df65f687accf68825374f15d440411c86913bd272b79f5d8dca0",
        "the pinned fields and canonical encoding retain their cache key"
    );
    assert_eq!(key, espressif_s3().key(), "pack key is repeatable");
    let mut changed = pack.clone();
    changed.files.reverse();
    changed.repositories.reverse();
    assert_eq!(
        changed.key(),
        key,
        "canonical key is independent of list order"
    );
    changed.import_map_version.push('2');
    assert_ne!(
        changed.key(),
        key,
        "import-map version contributes to the key"
    );
}

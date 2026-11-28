//! The part library on a made-up supplier: each number is fetched once, a newer converter converts from
//! the stored responses, a failed request leaves nothing, a refused part keeps its responses.

use fab_elec_import::library::{Converted, Fetch, ImportError, Library, Origin, Raw, Source};
use fab_elec_import::Issue;
use std::cell::Cell;
use std::path::PathBuf;

/// "example" parts: `E` and digits; two requests each (the part, then a model the part names).
struct Example {
    version: Cell<u32>,
}

impl Source for Example {
    fn folder(&self) -> &'static str {
        "example"
    }
    fn version(&self) -> u32 {
        self.version.get()
    }
    fn number(&self, text: &str) -> Result<String, Issue> {
        let t = text.trim().to_ascii_uppercase();
        match t.strip_prefix('E') {
            Some(d) if !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()) => Ok(t),
            _ => Err(Issue::new(0, "number", format!("{text:?} is not an example number"))),
        }
    }
    fn fetch(&self, number: &str, get: &mut dyn FnMut(&str) -> Result<Vec<u8>, ImportError>, raw: &mut Raw) -> Result<(), ImportError> {
        let part = get(&format!("https://parts.example/part/{number}"))?;
        let model = String::from_utf8_lossy(&part).split_whitespace().nth(1).unwrap_or("").to_string();
        if model.is_empty() {
            let url = format!("https://parts.example/part/{number}");
            return Err(ImportError::Response { number: number.into(), url, issue: Issue::new(0, "model", "names no model") });
        }
        raw.put("part.txt", part);
        raw.put("model.obj", get(&format!("https://parts.example/model/{model}"))?);
        Ok(())
    }
    fn convert(&self, number: &str, raw: &Raw) -> Result<Converted, Issue> {
        let part = String::from_utf8_lossy(raw.get("part.txt").unwrap_or_default()).into_owned();
        if part.starts_with("odd") && self.version.get() < 3 {
            return Err(Issue::new(1, "shape", "an odd shape this converter does not know"));
        }
        Ok(Converted {
            part: format!("// {number} by converter {}\n", self.version.get()),
            mesh: Some(raw.get("model.obj").unwrap_or_default().to_vec()),
            warnings: vec![Issue::new(2, "graphics", "drawn symbol graphics are left out")],
        })
    }
}

/// Serves "part E<n>": `<shape> m<n>`, counts every request, fails the urls it is told to.
#[derive(Default)]
struct Net {
    requests: usize,
    fail: Vec<String>,
}

impl Fetch for Net {
    fn get(&mut self, url: &str) -> Result<(u16, Vec<u8>), String> {
        self.requests += 1;
        if self.fail.iter().any(|f| url.ends_with(f.as_str())) {
            return Ok((500, Vec::new()));
        }
        if let Some(n) = url.strip_prefix("https://parts.example/part/") {
            let shape = if n == "E3" { "odd" } else { "chip" };
            return Ok((200, format!("{shape} m{}", &n[1..]).into_bytes()));
        }
        Ok((200, format!("o model {url}").into_bytes()))
    }
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fab-elec-import-library-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

#[test]
fn library_fetches_each_number_once() {
    let root = scratch();
    let lib = Library::new(&root);
    let src = Example { version: Cell::new(1) };
    let mut net = Net::default();

    // A miss fetches both responses and stores the converted part beside them.
    let e = lib.import(&src, " e1 ", &mut net).expect("first import");
    assert_eq!((e.origin, e.number.as_str(), net.requests), (Origin::Fetched, "E1", 2));
    assert_eq!(std::fs::read(e.dir.join("raw/part.txt")).unwrap(), b"chip m1", "responses kept exactly");
    assert!(e.part.as_ref().is_some_and(|p| p.ends_with("E1.part.splash")) && e.mesh.is_some());
    assert_eq!(e.warnings, vec!["line 2: graphics: drawn symbol graphics are left out".to_string()]);

    // A hit makes no request; a newer converter converts again from raw, still none.
    assert_eq!(lib.import(&src, "E1", &mut net).expect("hit").origin, Origin::Library);
    src.version.set(2);
    let e = lib.import(&src, "E1", &mut net).expect("reconvert");
    assert_eq!((e.origin, net.requests), (Origin::Reconverted, 2), "a converter upgrade never fetches");
    assert_eq!(std::fs::read_to_string(e.part.unwrap()).unwrap(), "// E1 by converter 2\n");

    // A failed request leaves no entry and no partial folder, and says which request.
    net.fail = vec!["model/m2".into()];
    let err = lib.import(&src, "E2", &mut net).expect_err("the model request fails");
    assert!(matches!(&err, ImportError::Request { number, url, message } if number == "E2" && url.ends_with("model/m2") && message == "status 500"), "{err}");
    let left: Vec<_> = std::fs::read_dir(root.join("example")).unwrap().flatten().map(|i| i.file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("E1")], "nothing of E2 is left");
    assert_eq!(net.requests, 4);

    // A refused conversion keeps the responses; a converter that knows the shape takes them, no request.
    let e = lib.import(&src, "E3", &mut net).expect("refused, kept");
    assert_eq!((e.origin, e.part.is_none(), net.requests), (Origin::Fetched, true, 6));
    assert_eq!(e.refused.as_deref(), Some("line 1: shape: an odd shape this converter does not know"));
    // A build reads the models the library holds: a refused part, a partial import and a failed
    // number have none.
    std::fs::create_dir_all(root.join("example/.partial-E9-1-0")).expect("a partial import's folder");
    let meshes = |lib: &Library| -> Vec<String> {
        lib.meshes(&src).into_iter().map(|(number, mesh)| {
            assert_eq!(mesh, root.join("example").join(&number).join(format!("{number}.glb")), "{number}: its entry's mesh file");
            number
        }).collect()
    };
    assert_eq!(meshes(&lib), ["E1"], "only E1 holds a mesh");
    src.version.set(3);
    let e = lib.import(&src, "E3", &mut net).expect("now converted");
    assert_eq!((e.origin, e.refused, e.part.is_some(), net.requests), (Origin::Reconverted, None, true, 6));
    assert!(!e.dir.join("refused.txt").exists());
    assert_eq!(meshes(&lib), ["E1", "E3"], "a converted part's mesh is listed, in number order");
    assert!(Library::new(root.join("none")).meshes(&src).is_empty(), "no library lists nothing");

    // Only a delete lets a number be fetched again; a bad number asks nothing.
    assert!(lib.delete(&src, "E1").unwrap());
    assert_eq!(lib.import(&src, "E1", &mut net).expect("refetch").origin, Origin::Fetched);
    assert_eq!(net.requests, 8);
    assert!(matches!(lib.import(&src, "X9", &mut net), Err(ImportError::Number(_))));
    assert!(matches!(lib.import(&src, "E../1", &mut net), Err(ImportError::Number(_))));
    assert_eq!(net.requests, 8);

    // The request log is the count: one line per request, failures included.
    let log = std::fs::read_to_string(root.join("requests.log")).unwrap();
    assert_eq!(log.lines().count(), net.requests);
    assert!(log.lines().any(|l| l.contains(" E2 GET https://parts.example/model/m2 500 0")), "{log}");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn utc_formats_civil_time() {
    use std::time::{Duration, UNIX_EPOCH};
    let at = |s: u64| fab_elec_import::library::utc(UNIX_EPOCH + Duration::from_secs(s));
    assert_eq!(at(0), "1970-01-01T00:00:00Z");
    assert_eq!(at(951_782_400), "2000-02-29T00:00:00Z", "a leap day");
    assert_eq!(at(1_794_745_296), "2026-11-15T12:21:36Z");
}

#[test]
fn library_parts_lists_local_splash_files_deterministically_without_network() {
    let root = std::env::temp_dir().join(format!("fab-library-local-parts-{}", std::process::id()));
    let folder = root.join("example");
    let src = Example { version: Cell::new(1) };
    for (name, file) in [("E7", "E7.part.splash"), ("E2", "E2.part.splash"), ("E3", "E3.part.splash"),
        ("E4.partial", "E4.partial.part.splash"), ("e5", "e5.part.splash"), ("E6", "E6.glb")] {
        let dir = folder.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(file), "").unwrap();
    }
    std::fs::write(folder.join("E3/refused.txt"), "conversion refused").unwrap();
    assert_eq!(Library::new(&root).parts(&src), ["E2", "E7"].map(|n| (n.into(), folder.join(n).join(format!("{n}.part.splash")))),
        "only canonical, non-refused part documents should be listed, in part-number order");
    std::fs::remove_dir_all(root).unwrap();
}

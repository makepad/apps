//! Imports parts into a part library through the library's own import path, with the network:
//! `fab-elec-partimport --out <dir> <number>...` (the library root; without `--out`, `$DEVHUB_KEEP`, the
//! keep folder of a devhub run), or `--delete <number>...` to remove entries. A number already in the
//! library makes no request; every request is a line of `<dir>/requests.log`.

use fab_elec_import::easyeda::Lcsc;
use fab_elec_import::library::{Fetch, Library};
use makepad_network::blocking_http::{request_no_redirect, Limits, Request};

/// HTTPS GETs through the platform's blocking client (generic client name, one hop, no body cap: a large
/// model is slow, never refused), counted.
struct Net {
    requests: usize,
}

impl Fetch for Net {
    fn get(&mut self, url: &str) -> Result<(u16, Vec<u8>), String> {
        self.requests += 1;
        let limits = Limits { max_body_bytes: usize::MAX, ..Limits::default() };
        let response = request_no_redirect(Request::get(url).limits(limits)).map_err(|e| e.to_string())?;
        Ok((response.status, response.body))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut library = None;
    let mut delete = false;
    let mut numbers = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--out" => library = it.next().cloned(),
            "--delete" => delete = true,
            _ => numbers.push(a.clone()),
        }
    }
    let library = library.or_else(|| std::env::var("DEVHUB_KEEP").ok().filter(|d| !d.is_empty()));
    let (Some(root), false) = (library, numbers.is_empty()) else {
        eprintln!("usage: fab-elec-partimport [--out <dir>] [--delete] <number>...  (default out: $DEVHUB_KEEP)");
        std::process::exit(2);
    };
    let lib = Library::new(root);
    let mut net = Net { requests: 0 };
    let mut failed = false;
    for n in &numbers {
        if delete {
            match lib.delete(&Lcsc, n) {
                Ok(had) => println!("{n}: {}", if had { "deleted" } else { "not in the library" }),
                Err(e) => {
                    failed = true;
                    println!("{n}: {e}");
                }
            }
            continue;
        }
        let before = net.requests;
        match lib.import(&Lcsc, n, &mut net) {
            Ok(e) => {
                println!("{}: {:?}, {} request(s), {}", e.number, e.origin, net.requests - before, e.dir.display());
                if let Some(r) = &e.refused {
                    println!("  refused: {r}");
                }
                for w in &e.warnings {
                    println!("  warning: {w}");
                }
            }
            Err(e) => {
                failed = true;
                println!("{n}: {e} ({} request(s))", net.requests - before);
            }
        }
    }
    println!("{} request(s) in all", net.requests);
    std::process::exit(if failed { 1 } else { 0 });
}

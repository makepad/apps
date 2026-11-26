//! The browser's files, fetched from the page: what an open read and the
//! source did not hold (`makepad_asset_data::source::store::take_wanted`)
//! is fetched relative to the page and installed in the source, and the
//! open runs again. Every reader reads through the source, so a page whose
//! files come packed in the wasm installs them the same way and fetches
//! nothing.
//!
//! A file comes in pieces of `PIECE` bytes (`Range: bytes=a-b`, answered
//! 206 with `Content-Range: bytes a-b/size`; a server that answers 200 sends
//! it whole). A directory is the URL with a trailing `/`, answered with its
//! listing, one entry a line: `f <size> <name>` or `d - <name>`. 404 means
//! there is nothing at the path.

use makepad_asset_data::source::store;
use makepad_asset_data::source::{Entry, EntryKind};
use makepad_widgets::*;
use std::collections::HashMap;
use std::path::Path;

/// The most one request asks for.
const PIECE: u64 = 1 << 20;

struct Want {
    path: String,
    dir: bool,
    got: Vec<u8>,
    embedded: bool,
}

#[derive(Default)]
pub struct Fetching {
    requests: HashMap<LiveId, Want>,
    /// Bytes come, and bytes known of (the loader's `X MB of Y MB`).
    got: u64,
    total: u64,
}

fn header<'a>(response: &'a HttpResponse, name: &str) -> Option<&'a str> {
    response.headers.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).and_then(|(_, values)| values.first()).map(String::as_str)
}

/// `bytes a-b/size` as (b + 1, size).
fn content_range(value: &str) -> Option<(u64, u64)> {
    let (span, size) = value.trim().strip_prefix("bytes ")?.split_once('/')?;
    let (_, last) = span.split_once('-')?;
    Some((last.trim().parse::<u64>().ok()? + 1, size.trim().parse().ok()?))
}

fn listing(text: &str) -> Vec<Entry> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let kind = match parts.next()? {
                "f" => EntryKind::File,
                "d" => EntryKind::Dir,
                _ => return None,
            };
            let _size = parts.next()?;
            let name = parts.next()?.to_string();
            (!name.is_empty() && !name.contains('/')).then_some(Entry { name, kind })
        })
        .collect()
}

impl Fetching {
    /// The bytes fetched, of those known to be on their way.
    pub fn progress(&self) -> (u64, u64) {
        (self.got, self.total)
    }

    /// Fetch what was wanted; true while anything is on its way (the open
    /// runs again once it has all come).
    pub fn fetch(&mut self, cx: &mut Cx, wanted: Vec<(String, bool)>) -> bool {
        for (path, dir) in wanted {
            if self.requests.values().any(|want| want.path == path && want.dir == dir) {
                continue;
            }
            let embedded = makepad_widgets::makepad_platform::os::web_embed::embedded().is_some_and(|embed| embed.get(&path).is_some());
            self.request(cx, Want { path, dir, got: Vec::new(), embedded });
        }
        !self.requests.is_empty()
    }

    fn request(&mut self, cx: &mut Cx, want: Want) {
        let url = match (want.dir, want.path.as_str()) {
            (true, "") => "./".to_string(),
            (true, path) => format!("{path}/"),
            (false, path) => path.to_string(),
        };
        let mut request = HttpRequest::new(url, HttpMethod::GET);
        // Files already in wasm are local reads, not network transfers. Read
        // them whole, so their byte size does not add fetch rounds or progress.
        if !want.dir && !want.embedded {
            let from = want.got.len() as u64;
            request.set_header("Range".into(), format!("bytes={from}-{}", from + PIECE - 1));
        }
        if !want.embedded { request.set_max_response_body_bytes(PIECE * 2); }
        let id = LiveId::unique();
        cx.http_request(id, request);
        self.requests.insert(id, want);
    }

    /// Take the replies to our fetches; true when the last one has come.
    pub fn handle_event(&mut self, cx: &mut Cx, event: &Event) -> bool {
        let Event::NetworkResponses(responses) = event else { return false };
        let mut took = false;
        for response in responses {
            let (id, result) = match response {
                NetworkResponse::HttpResponse { request_id, response } => (*request_id, Ok(response)),
                NetworkResponse::HttpError { request_id, error } => (*request_id, Err(error.message.clone())),
                _ => continue,
            };
            let Some(mut want) = self.requests.remove(&id) else { continue };
            took = true;
            let path = Path::new(&want.path).to_path_buf();
            let response = match result {
                Ok(response) => response,
                Err(error) => {
                    log!("makepad-story-doc-player: {}: {error}", want.path);
                    store::install_absent(&path);
                    continue;
                }
            };
            let body = response.body().unwrap_or(&[]);
            match (response.status_code, want.dir) {
                (200, true) => store::install_dir(&path, listing(&String::from_utf8_lossy(body))),
                (200, false) => {
                    if !want.embedded {
                        self.got += body.len() as u64;
                        self.total += body.len() as u64;
                    }
                    store::install_file(&path, body.to_vec())
                }
                (206, false) => {
                    if want.got.is_empty() {
                        if let Some((_, size)) = header(response, "content-range").and_then(content_range) {
                            self.total += size;
                        }
                    }
                    self.got += body.len() as u64;
                    want.got.extend_from_slice(body);
                    match header(response, "content-range").and_then(content_range) {
                        Some((end, size)) if end < size && !body.is_empty() => self.request(cx, want),
                        Some(_) => store::install_file(&path, std::mem::take(&mut want.got)),
                        None => {
                            log!("makepad-story-doc-player: {}: a 206 without a usable Content-Range", want.path);
                            store::install_absent(&path);
                        }
                    }
                }
                (404, _) => store::install_absent(&path),
                (status, _) => {
                    log!("makepad-story-doc-player: {}: HTTP {status}", want.path);
                    store::install_absent(&path);
                }
            }
        }
        took && self.requests.is_empty()
    }
}

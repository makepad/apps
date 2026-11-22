//! A resin printer that speaks the link's protocol ([`super`]) on the loopback address, for
//! tests. It holds the files it is sent in memory and checks a stack before it prints it, as a
//! printer would. It cures layers only when the test steps it ([`FakePrinter::step`]), and can
//! hold its answers to the files and to STATUS ([`FakePrinter::hold`]), so a test sees every
//! state without waiting on a clock. A test can give it a job as an earlier run of Fab left it
//! ([`FakePrinter::set_job`]).

use super::{file_name_ok, layer_files, State, MAX_FILE, VERSION};
use crate::export::{FORMAT, MANIFEST};
use makepad_strict_json::Value;
use makepad_zune_png::makepad_zune_core::bytestream::ZCursor;
use makepad_zune_png::PngDecoder;
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

/// What the fake printer holds and is doing.
struct Held {
    name: String,
    /// The images it shows: width and height in pixels.
    screen: [u32; 2],
    files: BTreeMap<String, Vec<u8>>,
    state: State,
    layer: u32,
    layers: u32,
    /// The connection breaks once this many more files have arrived.
    drop_after: Option<usize>,
    /// Every request line, as it came.
    requests: Vec<String>,
}

/// A printer on `127.0.0.1` at a port of its own. It serves one connection at a time and stops
/// when dropped.
pub struct FakePrinter {
    addr: SocketAddr,
    held: Arc<Mutex<Held>>,
    closing: Arc<AtomicBool>,
    /// The connection it serves: shut when it stops, so a client still connected can't hold it.
    current: Arc<Mutex<Option<TcpStream>>>,
    /// While true, a file that arrived and STATUS aren't answered.
    holding: Arc<(Mutex<bool>, Condvar)>,
    serving: Option<JoinHandle<()>>,
}

impl FakePrinter {
    /// A printer named `name` whose screen shows `screen` (width, height) pixel images.
    pub fn start(name: &str, screen: [u32; 2]) -> FakePrinter {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("a loopback port for the fake printer");
        let addr = listener.local_addr().expect("the fake printer's address");
        let held = Arc::new(Mutex::new(Held { name: name.to_string(), screen, files: BTreeMap::new(), state: State::Idle, layer: 0, layers: 0, drop_after: None, requests: Vec::new() }));
        let closing = Arc::new(AtomicBool::new(false));
        let current = Arc::new(Mutex::new(None));
        let holding = Arc::new((Mutex::new(false), Condvar::new()));
        let (h, c, now, hold) = (held.clone(), closing.clone(), current.clone(), holding.clone());
        let serving = std::thread::spawn(move || {
            for stream in listener.incoming() {
                if c.load(Ordering::SeqCst) {
                    break;
                }
                if let Ok(stream) = stream {
                    *now.lock().unwrap() = stream.try_clone().ok();
                    // a connection that breaks ends; the next one is served
                    let _ = serve(stream, &h, &hold);
                    *now.lock().unwrap() = None;
                }
            }
        });
        FakePrinter { addr, held, closing, current, holding, serving: Some(serving) }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The files it holds, by name.
    pub fn files(&self) -> BTreeMap<String, Vec<u8>> {
        self.held.lock().unwrap().files.clone()
    }

    /// Drops one file it holds, as if it never came.
    pub fn forget(&self, name: &str) {
        self.held.lock().unwrap().files.remove(name);
    }

    /// Every request line it was sent.
    pub fn requests(&self) -> Vec<String> {
        self.held.lock().unwrap().requests.clone()
    }

    /// Holds the answer to each file that arrives and to each STATUS from now on (the asker
    /// waits), or lets them go.
    pub fn hold(&self, on: bool) {
        *self.holding.0.lock().unwrap() = on;
        self.holding.1.notify_all();
    }

    /// The connection breaks once `files` more files have arrived.
    pub fn drop_after(&self, files: usize) {
        self.held.lock().unwrap().drop_after = Some(files);
    }

    /// Its job is in `state`, `layer` of its `layers` cured, as an earlier run of Fab left it (the
    /// files it holds stay as they are).
    pub fn set_job(&self, state: State, layer: u32, layers: u32) {
        let mut h = self.held.lock().unwrap();
        (h.state, h.layer, h.layers) = (state, layer, layers);
    }

    /// Cures `layers` more layers while it prints: the job is done at its last.
    pub fn step(&self, layers: u32) {
        let mut h = self.held.lock().unwrap();
        if h.state == State::Printing {
            h.layer = (h.layer + layers).min(h.layers);
            if h.layer == h.layers {
                h.state = State::Done;
            }
        }
    }
}

impl Drop for FakePrinter {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::SeqCst);
        self.hold(false);
        if let Some(s) = self.current.lock().unwrap().take() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
        // the accept loop wakes with this connection and sees it is closing
        let _ = TcpStream::connect(self.addr);
        if let Some(t) = self.serving.take() {
            let _ = t.join();
        }
    }
}

/// One connection, request by request, until it closes or says goodbye.
fn serve(stream: TcpStream, held: &Mutex<Held>, holding: &(Mutex<bool>, Condvar)) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(());
        }
        let line = line.trim_end_matches(['\n', '\r']).to_string();
        let (verb, rest) = line.split_once(' ').unwrap_or((line.as_str(), ""));
        held.lock().unwrap().requests.push(line.clone());
        let answer = match verb {
            "HELLO" if rest == VERSION.to_string() => format!("READY {}", held.lock().unwrap().name),
            "HELLO" => format!("ERR this printer speaks version {VERSION}"),
            "SEND" => {
                let parsed = rest.split_once(' ').and_then(|(name, len)| Some((name.to_string(), len.parse::<usize>().ok()?)));
                let Some((name, len)) = parsed.filter(|(n, len)| file_name_ok(n) && *len <= MAX_FILE) else {
                    // the payload's length is unknown: nothing after this line can be read
                    writer.write_all(format!("ERR {rest:?} is no file and length\n").as_bytes())?;
                    return Ok(());
                };
                let mut bytes = vec![0u8; len];
                reader.read_exact(&mut bytes)?;
                wait_held(holding);
                let mut h = held.lock().unwrap();
                if let Some(n) = h.drop_after.as_mut() {
                    if *n == 0 {
                        h.drop_after = None;
                        return Ok(());
                    }
                    *n -= 1;
                }
                h.files.insert(name, bytes);
                "OK".to_string()
            }
            "CLEAR" => {
                let mut h = held.lock().unwrap();
                if matches!(h.state, State::Printing | State::Paused) {
                    "ERR a job is printing".to_string()
                } else {
                    h.files.clear();
                    (h.state, h.layer, h.layers) = (State::Idle, 0, 0);
                    "OK".to_string()
                }
            }
            "PRINT" => {
                let mut h = held.lock().unwrap();
                if matches!(h.state, State::Printing | State::Paused) {
                    "ERR a job is printing".to_string()
                } else {
                    match check(&h.files, h.screen) {
                        Ok(layers) => {
                            (h.state, h.layer, h.layers) = (State::Printing, 0, layers);
                            "OK".to_string()
                        }
                        Err(why) => format!("ERR {why}"),
                    }
                }
            }
            "STATUS" => {
                wait_held(holding);
                let h = held.lock().unwrap();
                format!("STATUS {} {} {}", h.state.name(), h.layer, h.layers)
            }
            "PAUSE" | "RESUME" | "STOP" => {
                let mut h = held.lock().unwrap();
                match (verb, h.state) {
                    ("PAUSE", State::Printing) => {
                        h.state = State::Paused;
                        "OK".to_string()
                    }
                    ("RESUME", State::Paused) => {
                        h.state = State::Printing;
                        "OK".to_string()
                    }
                    ("STOP", State::Printing | State::Paused) => {
                        h.state = State::Stopped;
                        "OK".to_string()
                    }
                    (_, s) => format!("ERR the printer is {}", s.name()),
                }
            }
            "BYE" => return Ok(()),
            _ => format!("ERR no request {verb:?}"),
        };
        writer.write_all(format!("{answer}\n").as_bytes())?;
        writer.flush()?;
    }
}

/// Waits while the test holds the answers.
fn wait_held(holding: &(Mutex<bool>, Condvar)) {
    drop(holding.1.wait_while(holding.0.lock().unwrap(), |on| *on).unwrap());
}

/// Whether `files` are a stack this printer prints: its layers' count.
fn check(files: &BTreeMap<String, Vec<u8>>, screen: [u32; 2]) -> Result<u32, String> {
    let manifest = files.get(MANIFEST).ok_or("no manifest came")?;
    let m = makepad_strict_json::parse(manifest).map_err(|e| format!("the manifest is no JSON: {e}"))?;
    if m.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err("the manifest is no layer stack's".into());
    }
    let layers = layer_files(manifest)?;
    for file in &layers {
        let bytes = files.get(file).ok_or_else(|| format!("{file} is missing"))?;
        let mut d = PngDecoder::new(ZCursor::new(bytes));
        d.decode_headers().map_err(|e| format!("{file} is no PNG: {e:?}"))?;
        let (w, h) = d.dimensions().ok_or_else(|| format!("{file} has no size"))?;
        if [w as u32, h as u32] != screen {
            return Err(format!("{file} is {w} x {h} pixels, this screen is {} x {}", screen[0], screen[1]));
        }
    }
    Ok(layers.len() as u32)
}

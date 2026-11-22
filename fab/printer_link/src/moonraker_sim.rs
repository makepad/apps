//! In-memory Moonraker peer. Streams never open sockets or resolve names.
use crate::secrets::Secret;
use makepad_network::{ParsedWebSocketMessage, WebSocketMessageFormat, WebSocketMessageHeader, WebSocketParser};
use makepad_strict_json::{obj, parse, s, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex, Weak};

struct ReportFeed {
    feed: crate::sim::SimFeed,
    path: std::path::PathBuf,
    length: u64,
    record: usize,
}

struct Connection {
    input: Vec<u8>,
    output: VecDeque<u8>,
    websocket: bool,
    identified: bool,
    subscribed: Vec<String>,
    parser: WebSocketParser,
    dropped: bool,
    held: Option<(String, VecDeque<u8>)>,
}
impl Default for Connection {
    fn default() -> Self {
        Self { input: Vec::new(), output: VecDeque::new(), websocket: false, identified: false, subscribed: Vec::new(), parser: WebSocketParser::new(), dropped: false, held: None }
    }
}
struct State {
    files: BTreeMap<String, Vec<u8>>,
    klippy_state: String,
    api_key: Option<String>,
    refusal: Option<String>,
    status: Value,
    duration: f64,
    eventtime: f64,
    started: Vec<String>,
    started_path: Option<std::path::PathBuf>,
    connections: Vec<Weak<Mutex<Connection>>>,
    pongs: usize,
    holds: Vec<String>,
    drop_reply: Option<String>,
    start_state: String,
    requests: Vec<String>,
    subscriptions: usize,
    #[cfg(test)]
    upload: Option<(String, Vec<u8>)>,
}
/// Cloneable owner of persistent state shared by any number of streams.
#[derive(Clone)]
pub struct SimMoonraker { state: Arc<Mutex<State>>, reports: Option<Arc<Mutex<ReportFeed>>>, #[cfg(test)] changed: Arc<std::sync::Condvar> }
impl Default for SimMoonraker {
    fn default() -> Self { Self::new(None) }
}
/// The heads of the printer the simulator stands in for unless told otherwise.
pub const SIM_HEADS: usize = 4;
fn heater() -> Value { obj(vec![("temperature", Value::F64(20.0)), ("target", Value::F64(0.0))]) }
impl SimMoonraker {
    pub fn new(api_key: Option<String>) -> Self {
        Self::with_heads(api_key, SIM_HEADS)
    }
    /// A printer with `heads` heads, a spool on each.
    pub fn with_heads(api_key: Option<String>, heads: usize) -> Self {
        let status = vec![
            ("print_stats", obj(vec![("state", s("standby")), ("filename", s("")), ("print_duration", Value::F64(0.0)), ("message", s("")), ("info", obj(vec![("current_layer", Value::Int(0)), ("total_layer", Value::Int(10))]))])),
            ("virtual_sdcard", obj(vec![("progress", Value::F64(0.0)), ("is_active", Value::Bool(false)), ("file_path", s(""))])),
            ("webhooks", obj(vec![("state", s("ready")), ("state_message", s(""))])),
            ("heater_bed", heater()), ("toolhead", obj(vec![("extruder", s("extruder"))])),
            ("exclude_object", obj(vec![("objects", Value::Arr(vec![obj(vec![("name", s("cube"))])])), ("excluded_objects", Value::Arr(vec![])), ("current_object", Value::Null)])),
            ("print_task_config", obj(vec![
                ("filament_exist", Value::Arr(vec![Value::Bool(true); heads])),
                ("filament_type", Value::Arr(vec![s("PLA"); heads])),
                ("filament_color_rgba", Value::Arr(vec![s("FFFFFFFF"); heads])),
            ])), ("filament_detect", obj(vec![])),
        ];
        let mut status: Vec<(String, Value)> = status.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        status.extend((0..heads).map(|h| (crate::moonraker::heater_name(h), heater())));
        Self { state: Arc::new(Mutex::new(State { files: BTreeMap::new(), klippy_state: "ready".into(), api_key, refusal: None, status: Value::Obj(status), duration: 10.0, eventtime: 0.0, started: Vec::new(), started_path: None, connections: Vec::new(), pongs: 0, holds: Vec::new(), drop_reply: None, start_state: "printing".into(), requests: Vec::new(), subscriptions: 0, #[cfg(test)] upload: None })), reports: None, #[cfg(test)] changed: Arc::new(std::sync::Condvar::new()) }
    }
    pub(crate) fn from_reports(path: std::path::PathBuf) -> Self {
        let mut sim = Self::default();
        sim.state.lock().unwrap_or_else(|e| e.into_inner()).started_path = Some(crate::sim::started_path(&path));
        sim.reports = Some(Arc::new(Mutex::new(ReportFeed {
            feed: crate::sim::SimFeed::new(path.clone()), path, length: 0, record: 0,
        })));
        sim.follow_reports();
        sim
    }
    pub(crate) fn follow_reports(&self) {
        let Some(reports) = &self.reports else { return };
        let mut reports = reports.lock().unwrap_or_else(|e| e.into_inner());
        let Ok(length) = std::fs::metadata(&reports.path).map(|m| m.len()) else { return };
        if length < reports.length { reports.record = 0; }
        reports.length = length;
        let Ok(lines) = reports.feed.poll() else { return };
        let mut line_numbers = None;
        for line in lines {
            reports.record += 1;
            if self.report_line(&line) { continue; }
            // SimFeed omits comments and blanks. Recover the physical line number only for
            // a malformed record; never include any part of the record in a diagnostic.
            let numbers = line_numbers.get_or_insert_with(|| {
                std::fs::read(&reports.path).map(|bytes| {
                    bytes.split(|b| *b == b'\n').enumerate().filter(|(_, line)| {
                        let line = line.trim_ascii(); !line.is_empty() && !line.starts_with(b"#")
                    }).map(|(i, _)| i + 1).collect::<Vec<_>>()
                }).unwrap_or_default()
            });
            if let Some(number) = numbers.get(reports.record - 1) { eprintln!("fab: simulated report: skipped line {number}"); }
        }
    }
    fn report_line(&self, line: &str) -> bool {
        if let Some(reason) = line.strip_prefix("@refuse ").map(str::trim).filter(|s| !s.is_empty()) {
            self.refuse_next_start(reason);
            return true;
        }
        if let Some(head) = line.strip_prefix("@head ") {
            let fields: Vec<_> = head.split_whitespace().collect();
            if let [h, present, kind, rgba] = fields.as_slice() {
                if let Ok(h) = h.parse::<usize>() {
                    if h < 4 && matches!(*present, "0" | "1") && rgba.len() == 8 && rgba.bytes().all(|b| b.is_ascii_hexdigit()) {
                        self.set_head(h, *present == "1", kind, rgba);
                        return true;
                    }
                }
            }
            return false;
        }
        if let Ok(delta @ Value::Obj(_)) = parse(line.as_bytes()) {
            self.update(delta);
            return true;
        }
        false
    }
    pub fn stream(&self) -> SimStream {
        let connection = Arc::new(Mutex::new(Connection::default()));
        self.state.lock().unwrap_or_else(|e| e.into_inner()).connections.push(Arc::downgrade(&connection));
        SimStream { state: self.state.clone(), connection, #[cfg(test)] changed: self.changed.clone() }
    }
    /// Updates the filament reported by one physical head.
    pub fn set_head(&self, h: usize, exist: bool, kind: &str, rgba: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut config = state.status.get("print_task_config").cloned().unwrap_or(Value::Null);
        if let Value::Obj(fields) = &mut config {
            for (key, value) in [("filament_exist", Value::Bool(exist)), ("filament_type", s(kind)), ("filament_color_rgba", s(rgba))] {
                if let Some((_, Value::Arr(values))) = fields.iter_mut().find(|(k, _)| k == key) {
                    if let Some(slot) = values.get_mut(h) { *slot = value; }
                }
            }
        }
        let delta = obj(vec![("print_task_config", config)]);
        merge(&mut state.status, &delta);
        state.notify(&delta);
    }
    pub fn file(&self, name: &str) -> Option<Vec<u8>> { self.state.lock().unwrap_or_else(|e| e.into_inner()).files.get(name).cloned() }
    /// The objects the newest open connection subscribes to.
    #[cfg(test)]
    pub(crate) fn subscribed(&self) -> Vec<String> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.connections.iter().rev().find_map(Weak::upgrade).map(|c| c.lock().unwrap_or_else(|e| e.into_inner()).subscribed.clone()).unwrap_or_default()
    }
    pub fn started(&self) -> Vec<String> { self.state.lock().unwrap_or_else(|e| e.into_inner()).started.clone() }
    pub fn set_klippy_state(&self, firmware: &str) -> Result<(), &'static str> {
        self.firmware(firmware, "")
    }
    pub fn firmware(&self, firmware: &str, reason: &str) -> Result<(), &'static str> {
        if !matches!(firmware, "ready" | "startup" | "shutdown" | "error" | "disconnected") { return Err("invalid server state"); }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.klippy_state = firmware.into();
        let delta = obj(vec![("webhooks", obj(vec![("state", s(firmware)), ("state_message", s(reason))]))]);
        merge(&mut state.status, &delta);
        state.notify(&delta);
        let method = match firmware {
            "ready" => Some("notify_klippy_ready"), "shutdown" => Some("notify_klippy_shutdown"),
            "disconnected" => Some("notify_klippy_disconnected"), _ => None,
        };
        for connection in state.connections.iter().filter_map(Weak::upgrade) {
            let mut connection = connection.lock().unwrap_or_else(|e| e.into_inner());
            if firmware == "disconnected" { connection.subscribed.clear(); }
            if connection.websocket {
                if let Some(method) = method {
                    let text = obj(vec![("jsonrpc", s("2.0")), ("method", s(method)), ("params", Value::Arr(vec![]))]).to_json();
                    connection.output.extend(frame(text.as_bytes(), 1));
                }
            } else if firmware == "disconnected" && connection.held.as_ref().is_some_and(|(path, _)| path == "/printer/gcode/script") {
                connection.held = None;
                response(&mut connection, 503, obj(vec![]));
            }
        }
        Ok(())
    }
    pub fn restart(&self) {
        self.firmware("disconnected", "firmware restarting").unwrap();
        self.update(obj(vec![("print_stats", obj(vec![("state", s("standby")), ("filename", s(""))])),
            ("virtual_sdcard", obj(vec![("is_active", Value::Bool(false)), ("file_path", s(""))]))]));
        self.firmware("startup", "firmware starting").unwrap();
    }
    /// Hold a response after applying its request; releasing it never repeats the operation.
    pub fn hold_reply(&self, path: &str) { self.state.lock().unwrap_or_else(|e| e.into_inner()).holds.push(path.into()); }
    pub fn release_reply(&self, path: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.holds.retain(|p| p != path);
        for connection in state.connections.iter().filter_map(Weak::upgrade) {
            let mut connection = connection.lock().unwrap_or_else(|e| e.into_inner());
            if connection.held.as_ref().is_some_and(|(p, _)| p == path) {
                let (_, bytes) = connection.held.take().unwrap();
                connection.output.extend(bytes);
            }
        }
    }
    #[cfg(test)]
    pub(crate) fn ready_without_notification(&self) {
        let mut state = self.state.lock().unwrap();
        state.klippy_state = "ready".into();
        merge(&mut state.status, &obj(vec![("webhooks", obj(vec![("state", s("ready")), ("state_message", s(""))]))]));
    }
    #[cfg(test)]
    pub(crate) fn held(&self, path: &str) -> bool { self.state.lock().unwrap().holds.iter().any(|p| p == path) }
    #[cfg(test)]
    pub(crate) fn wait_requests(&self, path: &str, count: usize) {
        let state = self.state.lock().unwrap();
        let (state, _) = self.changed.wait_timeout_while(state, std::time::Duration::from_secs(5), |state| state.requests.iter().filter(|p| p.as_str() == path).count() < count).unwrap();
        assert!(state.requests.iter().filter(|p| p.as_str() == path).count() >= count, "request arrived: {path}");
    }
    pub fn drop_reply(&self, path: &str) { self.state.lock().unwrap_or_else(|e| e.into_inner()).drop_reply = Some(path.into()); }
    pub fn start_state(&self, next: &str) { self.state.lock().unwrap_or_else(|e| e.into_inner()).start_state = next.into(); }
    pub fn request_count(&self, path: &str) -> usize { self.state.lock().unwrap_or_else(|e| e.into_inner()).requests.iter().filter(|p| p.as_str() == path).count() }
    pub fn subscription_count(&self) -> usize { self.state.lock().unwrap_or_else(|e| e.into_inner()).subscriptions }
    pub fn refuse_next_start(&self, message: &str) { self.state.lock().unwrap_or_else(|e| e.into_inner()).refusal = Some(message.into()); }
    pub fn set_job(&self, total_layer: u32, duration: f64) -> Result<(), &'static str> {
        if !duration.is_finite() || duration <= 0.0 { return Err("invalid duration"); }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).duration = duration;
        self.update(obj(vec![("print_stats", obj(vec![("info", obj(vec![("total_layer", Value::Int(total_layer as i64))]))]))]));
        Ok(())
    }
    /// Publish arbitrary partial status to exercise optional fields and future protocol objects.
    pub fn update(&self, delta: Value) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        merge(&mut state.status, &delta);
        state.notify(&delta);
    }
    pub fn step(&self, dt: f64) {
        if !dt.is_finite() || dt <= 0.0 { return; }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.eventtime += dt;
        let stats = state.status.get("print_stats").unwrap_or(&Value::Null);
        if stats.get("state").and_then(Value::as_str) != Some("printing") { return; }
        let elapsed = stats.get("print_duration").and_then(number).unwrap_or(0.0);
        let total = stats.get("info").and_then(|v| v.get("total_layer")).and_then(Value::as_u64).unwrap_or(0);
        let elapsed = (elapsed + dt).min(state.duration);
        let progress = elapsed / state.duration;
        let layer = (progress * total as f64).floor() as i64;
        let delta = obj(vec![
            ("print_stats", obj(vec![("state", s(if progress >= 1.0 { "complete" } else { "printing" })), ("print_duration", Value::F64(elapsed)), ("info", obj(vec![("current_layer", Value::Int(layer))]))])),
            ("virtual_sdcard", obj(vec![("progress", Value::F64(progress))])),
        ]);
        merge(&mut state.status, &delta);
        state.notify(&delta);
    }
    /// Drops existing connections; newly created streams can reconnect to the same state.
    pub fn drop_streams(&self) {
        for connection in self.state.lock().unwrap_or_else(|e| e.into_inner()).connections.iter().filter_map(Weak::upgrade) { connection.lock().unwrap_or_else(|e| e.into_inner()).dropped = true; }
    }
    pub fn ping(&self, payload: &[u8]) -> Result<(), &'static str> {
        if payload.len() > 125 { return Err("ping payload too long"); }
        for connection in self.state.lock().unwrap_or_else(|e| e.into_inner()).connections.iter().filter_map(Weak::upgrade) {
            let mut connection = connection.lock().unwrap_or_else(|e| e.into_inner());
            if connection.websocket && !connection.dropped { connection.output.extend(frame(payload, 9)); }
        }
        Ok(())
    }
    pub fn close(&self) {
        for connection in self.state.lock().unwrap_or_else(|e| e.into_inner()).connections.iter().filter_map(Weak::upgrade) {
            let mut connection = connection.lock().unwrap_or_else(|e| e.into_inner());
            if connection.websocket { connection.output.extend(frame(&[], 8)); }
        }
    }
    pub fn pong_count(&self) -> usize { self.state.lock().unwrap_or_else(|e| e.into_inner()).pongs }
}
fn number(v: &Value) -> Option<f64> { match v { Value::Int(n) => Some(*n as f64), Value::F64(n) => Some(*n), _ => None } }
fn merge(target: &mut Value, delta: &Value) {
    if let (Value::Obj(target), Value::Obj(delta)) = (&mut *target, delta) {
        for (key, value) in delta {
            if let Some((_, old)) = target.iter_mut().find(|(k,_)| k == key) { merge(old, value); }
            else { target.push((key.clone(), value.clone())); }
        }
    } else { *target = delta.clone(); }
}
fn frame(data: &[u8], opcode: u8) -> Vec<u8> {
    let header = WebSocketMessageHeader::from_len(data.len(), WebSocketMessageFormat::Text, false);
    let mut bytes = WebSocketParser::build_message(header, data);
    bytes[0] = 0x80 | opcode;
    bytes
}
fn subset(status: &Value, names: &[String]) -> Value {
    Value::Obj(names.iter().filter_map(|name| status.get(name).map(|v| (name.clone(), v.clone()))).collect())
}
impl State {
    fn notify(&mut self, delta: &Value) {
        self.connections.retain(|weak| weak.strong_count() > 0);
        for connection in self.connections.iter().filter_map(Weak::upgrade) {
            let mut connection = connection.lock().unwrap_or_else(|e| e.into_inner());
            if connection.subscribed.is_empty() || connection.dropped { continue; }
            let delta = subset(delta, &connection.subscribed);
            if matches!(&delta, Value::Obj(v) if v.is_empty()) { continue; }
            let text = obj(vec![("jsonrpc", s("2.0")), ("method", s("notify_status_update")), ("params", Value::Arr(vec![delta, Value::F64(self.eventtime)]))]).to_json();
            connection.output.extend(frame(text.as_bytes(), 1));
        }
    }
    fn script(&mut self, line: &str) -> Result<Option<Value>, String> {
        if let Some(args) = line.strip_prefix("SDCARD_PRINT_FILE_WITH_PARAMETERS ") {
            if let Some(message) = self.refusal.take() { return Err(message); }
            let args = start_args(args).map_err(str::to_string)?;
            let name = args.get("FILENAME").ok_or("missing file name")?;
            if !self.files.contains_key(name) { return Err("file does not exist".into()); }
            let delta = obj(vec![
                ("print_stats", obj(vec![("state", s(if self.start_state == "reset" { "standby" } else { &self.start_state })), ("filename", s(if self.start_state == "reset" { "" } else { name })), ("print_duration", Value::F64(0.0)), ("info", obj(vec![("current_layer", Value::Int(0))]))])),
                ("virtual_sdcard", obj(vec![("progress", Value::F64(0.0)), ("is_active", Value::Bool(self.start_state == "printing")), ("file_path", s(if self.start_state == "reset" { "" } else { name }))])),
                ("exclude_object", obj(vec![("excluded_objects", Value::Arr(vec![])), ("current_object", Value::Null)])),
            ]);
            let line = self.api_key.as_ref().map_or_else(|| line.to_string(), |key| Secret::new(key.as_bytes().to_vec()).redact(line));
            if let Some(path) = &self.started_path {
                let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)
                    .map_err(|_| "cannot record the simulated start".to_string())?;
                writeln!(file, "{line}").map_err(|_| "cannot record the simulated start".to_string())?;
            }
            self.started.push(line);
            return Ok(Some(delta));
        }
        if line == "SDCARD_RESET_FILE" {
            return Ok(Some(obj(vec![("print_stats", obj(vec![("state", s("standby")), ("filename", s(""))])),
                ("virtual_sdcard", obj(vec![("is_active", Value::Bool(false)), ("file_path", s(""))]))])));
        }
        if let Some(name) = line.strip_prefix("EXCLUDE_OBJECT NAME=") {
            let name = name.trim().trim_matches('"');
            if name.is_empty() || name.chars().any(char::is_control) { return Err("invalid object name".into()); }
            let mut names = self.status.get("exclude_object").and_then(|v| v.get("excluded_objects")).and_then(Value::as_arr).unwrap_or(&[]).to_vec();
            if !names.iter().any(|v| v.as_str() == Some(name)) { names.push(s(name)); }
            return Ok(Some(obj(vec![("exclude_object", obj(vec![("excluded_objects", Value::Arr(names))]))])));
        }
        Ok(None)
    }
}
fn start_args(mut text: &str) -> Result<BTreeMap<String, String>, &'static str> {
    let mut args = BTreeMap::new();
    while !text.is_empty() {
        let (key, rest) = text.split_once("=\"").ok_or("invalid start parameter")?;
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') { return Err("invalid start key"); }
        let (value, rest) = rest.split_once('"').ok_or("invalid start value")?;
        if value.chars().any(char::is_control) || args.insert(key.into(), value.into()).is_some() { return Err("invalid start value"); }
        text = if rest.is_empty() { rest } else { rest.strip_prefix(' ').ok_or("invalid start separator")? };
    }
    Ok(args)
}

/// One duplex in-memory stream. Empty reads return WouldBlock, dropped reads return EOF.
pub struct SimStream { state: Arc<Mutex<State>>, connection: Arc<Mutex<Connection>>, #[cfg(test)] changed: Arc<std::sync::Condvar> }
impl Read for SimStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        if buffer.is_empty() || connection.dropped { return Ok(0); }
        if connection.output.is_empty() { return Err(io::ErrorKind::WouldBlock.into()); }
        let count = buffer.len().min(connection.output.len());
        for byte in &mut buffer[..count] { *byte = connection.output.pop_front().ok_or(io::ErrorKind::UnexpectedEof)?; }
        Ok(count)
    }
}
impl Write for SimStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
        if connection.dropped { return Err(io::ErrorKind::BrokenPipe.into()); }
        connection.input.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        let mut deltas = Vec::new();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut connection = self.connection.lock().unwrap_or_else(|e| e.into_inner());
            if connection.dropped { return Err(io::ErrorKind::BrokenPipe.into()); }
            if connection.websocket { process_frames(&mut connection, &mut state)?; }
            else { while let Some(request) = request(&mut connection.input)? {
                state.requests.push(request.path.clone());
                if let Some(delta) = answer(&request, &mut connection, &mut state)? { merge(&mut state.status, &delta); deltas.push(delta); }
                delay_reply(&request.path, &mut connection, &mut state);
            } }
        }
        #[cfg(test)] self.changed.notify_all();
        for delta in deltas { self.state.lock().unwrap_or_else(|e| e.into_inner()).notify(&delta); }
        Ok(())
    }
}
struct Request { method: String, path: String, fields: BTreeMap<String,String>, body: Vec<u8> }
fn invalid() -> io::Error { io::ErrorKind::InvalidData.into() }
fn request(input: &mut Vec<u8>) -> io::Result<Option<Request>> {
    let Some(end) = input.windows(4).position(|w| w == b"\r\n\r\n") else {
        if input.len() > 32 * 1024 { return Err(invalid()); }
        return Ok(None);
    };
    if end + 4 > 32 * 1024 { return Err(invalid()); }
    let text = std::str::from_utf8(&input[..end]).map_err(|_| invalid())?;
    let mut lines = text.split("\r\n");
    let mut words = lines.next().ok_or_else(invalid)?.split_whitespace();
    let method = words.next().ok_or_else(invalid)?.to_string();
    let path = words.next().ok_or_else(invalid)?.to_string();
    if words.next() != Some("HTTP/1.1") || words.next().is_some() { return Err(invalid()); }
    let mut fields = BTreeMap::new();
    for line in lines {
        let (key, value) = line.split_once(':').ok_or_else(invalid)?;
        if fields.insert(key.to_ascii_lowercase(), value.trim().to_string()).is_some() { return Err(invalid()); }
    }
    let length = fields.get("content-length").map(|v| v.parse::<usize>()).transpose().map_err(|_| invalid())?.unwrap_or(0);
    let total = (end + 4).checked_add(length).ok_or_else(invalid)?;
    if input.len() < total { return Ok(None); }
    let body = input[end+4..total].to_vec();
    input.drain(..total);
    Ok(Some(Request { method, path, fields, body }))
}
fn response(connection: &mut Connection, code: u16, value: Value) {
    let body = value.to_json();
    let head = format!("HTTP/1.1 {code} Response\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: keep-alive\r\n\r\n", body.len());
    connection.output.extend(head.bytes()); connection.output.extend(body.bytes());
}
fn delay_reply(path: &str, connection: &mut Connection, state: &mut State) {
    if state.drop_reply.as_deref() == Some(path) {
        state.drop_reply = None;
        connection.output.clear();
        connection.dropped = true;
    } else if state.holds.iter().any(|p| p == path) {
        connection.held = Some((path.into(), std::mem::take(&mut connection.output)));
    }
}
fn answer(request: &Request, connection: &mut Connection, state: &mut State) -> io::Result<Option<Value>> {
    if state.api_key.as_ref().is_some_and(|key| request.fields.get("x-api-key") != Some(key)) {
        response(connection, 401, obj(vec![])); return Ok(None);
    }
    if request.method == "GET" && request.path == "/websocket" {
        let key = request.fields.get("sec-websocket-key").ok_or_else(invalid)?;
        if request.fields.get("sec-websocket-version").map(String::as_str) != Some("13") || !request.fields.get("upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket")) || !request.fields.get("connection").is_some_and(|v| v.eq_ignore_ascii_case("upgrade")) { return Err(invalid()); }
        connection.output.extend(WebSocketParser::create_upgrade_response(key).bytes());
        connection.websocket = true;
        return Ok(None);
    }
    let mut delta = None;
    let result = match (request.method.as_str(), request.path.as_str()) {
        ("DELETE", path) if path.starts_with("/server/files/gcodes/") => {
            let encoded = path.trim_start_matches("/server/files/gcodes/");
            let mut bytes = Vec::new();
            let mut rest = encoded.as_bytes();
            while let Some((&byte, tail)) = rest.split_first() {
                if byte == b'%' {
                    let hex = rest.get(1..3).and_then(|b| std::str::from_utf8(b).ok()).ok_or_else(invalid)?;
                    bytes.push(u8::from_str_radix(hex, 16).map_err(|_| invalid())?);
                    rest = &rest[3..];
                } else { bytes.push(byte); rest = tail; }
            }
            let name = String::from_utf8(bytes).map_err(|_| invalid())?;
            let stats = state.status.get("print_stats").unwrap_or(&Value::Null);
            if state.klippy_state == "ready" && matches!(stats.get("state").and_then(Value::as_str), Some("printing" | "paused")) && stats.get("filename").and_then(Value::as_str) == Some(&name) {
                response(connection, 403, obj(vec![("error", obj(vec![("message", s("File currently in use"))]))]));
                return Ok(None);
            }
            if state.files.remove(&name).is_none() { Err("Invalid file path".into()) }
            else { Ok(obj(vec![("action", s("delete_file")), ("item", obj(vec![("path", s(name))]))])) }
        }
        ("GET", "/server/files/list?root=gcodes") => Ok(Value::Arr(state.files.iter().map(|(name, bytes)| obj(vec![("path", s(name)), ("size", Value::Int(bytes.len() as i64))])).collect())),
        ("GET", "/server/info") => Ok(obj(vec![("klippy_state", s(&state.klippy_state)), ("moonraker_version", s("sim-1"))])),
        ("POST", "/server/files/upload") => {
            let content_type = request.fields.get("content-type").ok_or_else(invalid)?;
            #[cfg(test)] { state.upload = Some((content_type.clone(), request.body.clone())); }
            match multipart(content_type, &request.body) {
                Ok((name, bytes)) => {
                    state.files.insert(name.clone(), bytes);
                    response(connection, 201, obj(vec![("result", obj(vec![("item", obj(vec![("path", s(name))]))]))]));
                    return Ok(None);
                }
                Err(message) => Err(message.to_string()),
            }
        }
        ("POST", "/printer/gcode/script") => {
            let value = parse(&request.body).map_err(|_| invalid())?;
            let line = value.get("script").and_then(Value::as_str).ok_or_else(invalid)?;
            state.script(line).map(|change| { delta = change; s("ok") })
        }
        ("POST", "/printer/print/pause" | "/printer/print/resume" | "/printer/print/cancel") => {
            let old = state.status.get("print_stats").and_then(|v| v.get("state")).and_then(Value::as_str).unwrap_or("standby");
            let next = match request.path.as_str() {
                "/printer/print/pause" if old == "printing" => Some("paused"),
                "/printer/print/resume" if old == "paused" => Some("printing"),
                "/printer/print/cancel" => Some("cancelled"),
                _ => None,
            };
            if let Some(next) = next { delta = Some(obj(vec![("print_stats", obj(vec![("state", s(next))]))])); Ok(s("ok")) }
            else { Err("invalid print state".into()) }
        }
        ("GET", path) if path.starts_with("/printer/objects/query?") => {
            let names: Vec<_> = path.trim_start_matches("/printer/objects/query?").split('&').map(str::to_string).collect();
            Ok(obj(vec![("status", subset(&state.status, &names))]))
        }
        _ => { response(connection, 404, obj(vec![])); return Ok(None); }
    };
    match result {
        Ok(result) => response(connection, 200, obj(vec![("result", result)])),
        Err(message) => response(connection, 400, obj(vec![("error", obj(vec![("message", s(message))]))])),
    }
    Ok(delta)
}
fn multipart(content_type: &str, body: &[u8]) -> Result<(String,Vec<u8>), &'static str> {
    let boundary = content_type.strip_prefix("multipart/form-data; boundary=").filter(|v| !v.is_empty()).ok_or("missing multipart boundary")?;
    let prefix = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"root\"\r\n\r\ngcodes\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"print\"\r\n\r\nfalse\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"");
    let body = body.strip_prefix(prefix.as_bytes()).ok_or("invalid multipart part order")?;
    let end = body.iter().position(|b| *b == b'"').ok_or("missing file name")?;
    let name = std::str::from_utf8(&body[..end]).map_err(|_| "invalid file name")?;
    if name.contains('/') || name.contains('\\') || name.contains("..") || name.chars().any(char::is_control) || !name.ends_with(".gcode") { return Err("invalid file name"); }
    let bytes = body[end..].strip_prefix(b"\"\r\nContent-Type: application/octet-stream\r\n\r\n").ok_or("invalid file part")?;
    let suffix = format!("\r\n--{boundary}--\r\n");
    let bytes = bytes.strip_suffix(suffix.as_bytes()).ok_or("missing multipart ending")?;
    if bytes.windows(boundary.len()).any(|w| w == boundary.as_bytes()) { return Err("boundary in file"); }
    Ok((name.into(), bytes.to_vec()))
}
fn process_frames(connection: &mut Connection, state: &mut State) -> io::Result<()> {
    let mut messages = Vec::new();
    let mut pongs = 0;
    let mut failed = false;
    // Only complete masked client frames enter the shared parser.
    loop {
        let bytes = &connection.input;
        if bytes.len() < 2 { break; }
        if bytes[1] & 128 == 0 || bytes[0] & 128 == 0 || bytes[0] & 0x70 != 0 { return Err(invalid()); }
        let (length, header) = match bytes[1] & 127 {
            126 if bytes.len() < 4 => break,
            126 => (u16::from_be_bytes([bytes[2],bytes[3]]) as usize, 8),
            127 if bytes.len() < 10 => break,
            127 => (usize::try_from(u64::from_be_bytes(bytes[2..10].try_into().map_err(|_| invalid())?)).map_err(|_| invalid())?, 14),
            n => (n as usize, 6),
        };
        let total = length.checked_add(header).ok_or_else(invalid)?;
        if bytes.len() < total { break; }
        let bytes: Vec<_> = connection.input.drain(..total).collect();
        connection.parser.parse(&bytes, |event| match event {
            Ok(ParsedWebSocketMessage::Text(text)) => messages.push(text.to_string()),
            Ok(ParsedWebSocketMessage::Pong(_)) => pongs += 1,
            _ => failed = true,
        });
    }
    if failed { return Err(invalid()); }
    state.pongs += pongs;
    for text in messages {
        let value = parse(text.as_bytes()).map_err(|_| invalid())?;
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") { return Err(invalid()); }
        let id = value.get("id").cloned().ok_or_else(invalid)?;
        let params = value.get("params").ok_or_else(invalid)?;
        let result = match value.get("method").and_then(Value::as_str) {
            Some("server.connection.identify") => {
                if state.api_key.as_ref().is_some_and(|key| params.get("api_key").and_then(Value::as_str) != Some(key.as_str())) { return Err(io::ErrorKind::PermissionDenied.into()); }
                if params.get("client_name").and_then(Value::as_str) != Some("Fab") || params.get("version").and_then(Value::as_str) != Some(env!("CARGO_PKG_VERSION")) || params.get("type").and_then(Value::as_str) != Some("desktop") || params.get("url").and_then(Value::as_str) != Some("") { return Err(invalid()); }
                connection.identified = true;
                obj(vec![("connection_id", Value::Int(1))])
            }
            Some("printer.objects.subscribe") if connection.identified => {
                let Some(Value::Obj(objects)) = params.get("objects") else { return Err(invalid()); };
                if objects.iter().any(|(_,v)| !v.is_null()) { return Err(invalid()); }
                state.subscriptions += 1;
                connection.subscribed = objects.iter().map(|(k,_)| k.clone()).collect();
                obj(vec![("eventtime", Value::F64(state.eventtime)), ("status", subset(&state.status, &connection.subscribed))])
            }
            _ => return Err(invalid()),
        };
        let reply = obj(vec![("jsonrpc", s("2.0")), ("id", id), ("result", result)]).to_json();
        connection.output.extend(frame(reply.as_bytes(), 1));
        if value.get("method").and_then(Value::as_str) == Some("printer.objects.subscribe") { delay_reply("printer.objects.subscribe", connection, state); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use crate::moonraker::{objects, start_line, refusal, Client, Error, StartParams, StatusSocket};
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn upload_preserves_bytes_parts_and_progress_and_cancellation_discards() {
        let sim = SimMoonraker::default();
        let mut client = Client::new(sim.stream(), "memory", None).unwrap();
        let bytes = [b"fab-candidate\n".as_slice(), &vec![0xff; 40_000]].concat();
        let mut progress = Vec::new();
        let cancel = AtomicBool::new(false);
        assert_eq!(client.upload("cube.gcode", &bytes, &cancel, |n,total| progress.push((n,total))).unwrap(), "cube.gcode");
        assert_eq!(sim.file("cube.gcode").unwrap(), bytes);
        assert_eq!(progress.last(), Some(&(bytes.len(), bytes.len())));
        assert!(progress.windows(2).all(|p| p[0].0 < p[1].0));
        let state = sim.state.lock().unwrap_or_else(|e| e.into_inner());
        let (content_type, body) = state.upload.as_ref().unwrap();
        let boundary = content_type.split_once("boundary=").unwrap().1;
        assert!(!bytes.windows(boundary.len()).any(|w| w == boundary.as_bytes()));
        let text = String::from_utf8_lossy(body);
        assert!(text.find("name=\"root\"").unwrap() < text.find("name=\"print\"").unwrap());
        assert!(text.find("name=\"print\"").unwrap() < text.find("name=\"file\"").unwrap());
        drop(state);
        assert_eq!(client.upload("cancel.gcode", &bytes, &cancel, |n,_| { if n > 0 { cancel.store(true, Ordering::Release); } }), Err(Error::Cancelled));
        assert!(sim.file("cancel.gcode").is_none());
        assert!(client.server_info().is_err(), "unfinished upload stream cannot be reused");
        let mut client = Client::new(sim.stream(), "memory", None).unwrap();
        cancel.store(false, Ordering::Release);
        assert_eq!(client.upload("empty.gcode", b"", &cancel, |n,t| assert_eq!((n,t),(0,0))).unwrap(), "empty.gcode");
    }

    #[test]
    fn start_query_refusal_and_missing_file_follow_the_protocol() {
        let sim = SimMoonraker::default();
        let mut client = Client::new(sim.stream(), "memory", None).unwrap();
        let line = start_line("cube.gcode", &StartParams::default()).unwrap();
        assert!(matches!(client.script(&line), Err(Error::Refused(_))));
        client.upload("cube.gcode", b"G1 X1", &AtomicBool::new(false), |_,_| {}).unwrap();
        sim.refuse_next_start("531-16");
        let Error::Refused(message) = client.script(&line).unwrap_err() else { panic!("expected refusal"); };
        assert_eq!(refusal(&message), Some("the printer is busy"));
        assert!(sim.started().is_empty());
        client.script(&line).unwrap();
        assert_eq!(sim.started(), [line]);
        let status = client.query(&["print_stats", "heater_bed"]).unwrap();
        assert_eq!(status.get("print_stats").unwrap().get("state").unwrap().as_str(), Some("printing"));
        assert!(status.get("heater_bed").is_some());
        assert!(status.get("extruder").is_none());
        sim.set_klippy_state("shutdown").unwrap();
        assert_eq!(client.server_info().unwrap().klippy_state, "shutdown");
    }

    #[test]
    fn subscribed_status_tracks_controls_completion_exclusion_and_ping() {
        let sim = SimMoonraker::default();
        let mut socket = StatusSocket::connect(sim.stream(), "memory", None, SIM_HEADS).unwrap();
        let status = socket.poll().unwrap().unwrap();
        assert_eq!(status.state, "standby"); assert_eq!(status.extruders.len(), 4);
        assert_eq!(status.extruders[3].temperature, 20.0);
        let subscribed = sim.state.lock().unwrap_or_else(|e| e.into_inner()).connections[0].upgrade().unwrap();
        assert_eq!(subscribed.lock().unwrap_or_else(|e| e.into_inner()).subscribed, objects(SIM_HEADS));
        assert!(socket.poll().unwrap().is_none());
        let mut client = Client::new(sim.stream(), "memory", None).unwrap();
        client.upload("cube.gcode", b"G1 X1", &AtomicBool::new(false), |_,_| {}).unwrap();
        let line = start_line("cube.gcode", &StartParams::default()).unwrap();
        client.script(&line).unwrap();
        assert_eq!(socket.poll().unwrap().unwrap().state, "printing");
        sim.step(2.0);
        let status = socket.poll().unwrap().unwrap();
        assert!(status.progress > 0.0 && status.current_layer > 0);
        assert_eq!(status.total_layer, 10);
        client.pause().unwrap();
        assert_eq!(socket.poll().unwrap().unwrap().state, "paused");
        sim.step(2.0); assert!(socket.poll().unwrap().is_none());
        client.resume().unwrap();
        assert_eq!(socket.poll().unwrap().unwrap().state, "printing");
        client.script("EXCLUDE_OBJECT NAME=cube").unwrap();
        assert_eq!(socket.poll().unwrap().unwrap().excluded_objects, ["cube"]);
        sim.ping(b"same payload").unwrap(); assert!(socket.poll().unwrap().is_none());
        assert_eq!(sim.pong_count(), 1);
        sim.step(8.0);
        let status = socket.poll().unwrap().unwrap();
        assert_eq!((status.state.as_str(), status.progress, status.current_layer), ("complete",1.0,10));
        client.script(&line).unwrap(); socket.poll().unwrap();
        client.cancel().unwrap(); assert_eq!(socket.poll().unwrap().unwrap().state, "cancelled");
        sim.step(10.0); assert!(socket.poll().unwrap().is_none());
        sim.close(); assert!(socket.poll().unwrap().is_none()); assert!(socket.is_closed());
    }

    #[test]
    fn streams_drop_and_reconnect_without_losing_state() {
        let sim = SimMoonraker::default();
        let mut stream = sim.stream();
        assert_eq!(stream.read(&mut [0;1]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        let mut socket = StatusSocket::connect(sim.stream(), "memory", None, SIM_HEADS).unwrap();
        socket.poll().unwrap();
        let mut client = Client::new(sim.stream(), "memory", None).unwrap();
        client.upload("cube.gcode", b"G1 X1", &AtomicBool::new(false), |_,_| {}).unwrap();
        client.script(&start_line("cube.gcode", &StartParams::default()).unwrap()).unwrap();
        sim.step(3.0);
        let before = socket.poll().unwrap().unwrap().clone();
        assert_eq!(before.state, "printing");
        assert!(before.progress > 0.0 && before.progress < 1.0 && before.current_layer > 0);
        sim.drop_streams();
        assert_eq!(stream.read(&mut [0;1]).unwrap(), 0);
        assert!(stream.write(b"x").is_err());
        assert!(socket.poll().unwrap().is_none()); assert!(socket.is_closed());
        let mut replacement = StatusSocket::connect(sim.stream(), "memory", None, SIM_HEADS).unwrap();
        let after = replacement.poll().unwrap().unwrap();
        assert_eq!(sim.file("cube.gcode").unwrap(), b"G1 X1");
        assert_eq!(after.state, "printing");
        assert_eq!(after.filename, "cube.gcode");
        assert_eq!((after.progress, after.current_layer), (before.progress, before.current_layer));
    }

    #[test]
    fn partial_frames_fragments_and_unmatched_replies_preserve_pending_state() {
        let sim = SimMoonraker::default();
        let stream = sim.stream();
        let connection = stream.connection.clone();
        let mut socket = StatusSocket::connect(stream, "memory", None, SIM_HEADS).unwrap();
        let identify = connection.lock().unwrap_or_else(|e| e.into_inner()).output.drain(..).collect::<Vec<_>>();
        let unrelated = obj(vec![("jsonrpc",s("2.0")),("id",Value::Int(999)),("result",Value::Null)]).to_json();
        connection.lock().unwrap_or_else(|e| e.into_inner()).output.extend(frame(unrelated.as_bytes(),1));
        assert!(socket.poll().unwrap().is_none());
        connection.lock().unwrap_or_else(|e| e.into_inner()).output.extend(identify);
        assert_eq!(socket.poll().unwrap().unwrap().state, "standby");
        let text = obj(vec![("jsonrpc",s("2.0")),("method",s("notify_status_update")),("params",Value::Arr(vec![obj(vec![("print_stats",obj(vec![("message",s("partial"))]))])]))]).to_json();
        let middle = text.len()/2;
        let mut first = frame(&text.as_bytes()[..middle],1); first[0] &= 127;
        let last = frame(&text.as_bytes()[middle..],0);
        connection.lock().unwrap_or_else(|e| e.into_inner()).output.extend(&first[..1]);
        assert!(socket.poll().unwrap().is_none());
        connection.lock().unwrap_or_else(|e| e.into_inner()).output.extend(&first[1..]);
        sim.ping(b"inside fragments").unwrap();
        assert!(socket.poll().unwrap().is_none()); assert_eq!(sim.pong_count(),1);
        connection.lock().unwrap_or_else(|e| e.into_inner()).output.extend(&last[..last.len()-1]);
        assert!(socket.poll().unwrap().is_none());
        connection.lock().unwrap_or_else(|e| e.into_inner()).output.push_back(*last.last().unwrap());
        assert_eq!(socket.poll().unwrap().unwrap().message, "partial");
    }
    #[test]
    fn echoed_credentials_are_redacted_in_status_queries_refusals_and_recorded_lines() {
        let key = "private-test-key";
        let sim = SimMoonraker::new(Some(key.into()));
        sim.update(obj(vec![
            ("print_stats", obj(vec![("message", s(format!("echo {key}")))])),
            ("print_task_config", obj(vec![(key, Value::Arr(vec![s(key)]))])),
            ("filament_detect", s(key)),
        ]));
        let secret = || Some(Secret::new(key.as_bytes().to_vec()));
        let mut client = Client::new(sim.stream(), "memory", secret()).unwrap();
        let value = client.query(&["print_stats", "print_task_config", "filament_detect"]).unwrap();
        assert!(!format!("{value:?} {}", value.to_json()).contains(key));
        assert_eq!(value.get("print_task_config").unwrap().get("***"), Some(&Value::Arr(vec![s("***")])));
        let mut socket = StatusSocket::connect(sim.stream(), "memory", secret(), SIM_HEADS).unwrap();
        let status = socket.poll().unwrap().unwrap();
        assert!(!format!("{status:?} {} {}", status.print_task_config.to_json(), status.filament_detect.to_json()).contains(key));
        assert_eq!(status.message, "echo ***");
        sim.update(obj(vec![("print_task_config", s(key)), ("print_stats", obj(vec![("message", s(key))]))]));
        let status = socket.poll().unwrap().unwrap();
        assert!(!format!("{status:?} {}", status.print_task_config.to_json()).contains(key));
        assert_eq!(status.print_task_config, s("***"));
        client.upload("cube.gcode", b"G1 X1", &AtomicBool::new(false), |_,_| {}).unwrap();
        let line = start_line("cube.gcode", &StartParams { filament_types: vec![key.into()], ..Default::default() }).unwrap();
        sim.refuse_next_start(&format!("531-16 {key}"));
        let error = client.script(&line).unwrap_err();
        assert!(matches!(error, Error::Refused(_)));
        assert!(!format!("{error} {error:?}").contains(key));
        client.script(&line).unwrap();
        assert_eq!(sim.started(), [line.replace(key, "***")]);
    }

    struct Throttled {
        inner: SimStream,
        budget: Rc<std::cell::Cell<usize>>,
        fail: Rc<std::cell::Cell<bool>>,
    }
    impl Read for Throttled {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> { self.inner.read(bytes) }
    }
    impl Write for Throttled {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.fail.get() { return Err(io::ErrorKind::BrokenPipe.into()); }
            let count = self.budget.get().min(bytes.len());
            if count == 0 { return Err(io::ErrorKind::WouldBlock.into()); }
            let sent = self.inner.write(&bytes[..count])?;
            self.budget.set(self.budget.get() - sent);
            Ok(sent)
        }
        fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
    }
    #[test]
    fn pending_subscription_and_pong_frames_keep_their_order_and_ids() {
        let sim = SimMoonraker::default();
        let budget = Rc::new(std::cell::Cell::new(usize::MAX));
        let fail = Rc::new(std::cell::Cell::new(false));
        let stream = Throttled { inner: sim.stream(), budget: budget.clone(), fail: fail.clone() };
        let connection = stream.inner.connection.clone();
        let mut socket = StatusSocket::connect(stream, "memory", None, SIM_HEADS).unwrap();
        budget.set(3);
        assert!(socket.poll().unwrap().is_none());
        assert!(connection.lock().unwrap_or_else(|e| e.into_inner()).subscribed.is_empty());
        sim.ping(b"queued behind subscription").unwrap();
        assert!(socket.poll().unwrap().is_none());
        assert_eq!(sim.pong_count(), 0);
        budget.set(usize::MAX);
        assert_eq!(socket.poll().unwrap().unwrap().state, "standby", "reply must match the queued subscription id");
        assert_eq!(connection.lock().unwrap_or_else(|e| e.into_inner()).subscribed, objects(SIM_HEADS));
        assert_eq!(sim.pong_count(), 1, "the pong follows the complete subscription frame");
        sim.ping(b"partial pong").unwrap();
        budget.set(3);
        assert!(socket.poll().unwrap().is_none());
        fail.set(true);
        assert!(matches!(socket.poll(), Err(Error::Io)));
        assert!(socket.is_closed());
    }

    #[test]
    fn subscription_snapshot_orders_buffered_deltas_by_eventtime() {
        let sim = SimMoonraker::default();
        sim.step(10.0);
        sim.update(obj(vec![("virtual_sdcard", obj(vec![("progress", Value::F64(0.5))]))]));
        let budget = Rc::new(std::cell::Cell::new(usize::MAX));
        let stream = Throttled { inner: sim.stream(), budget: budget.clone(), fail: Default::default() };
        let connection = stream.inner.connection.clone();
        let mut socket = StatusSocket::connect(stream, "memory", None, SIM_HEADS).unwrap();
        budget.set(3);
        assert!(socket.poll().unwrap().is_none());
        for (progress, time) in [(0.75, Some(12.0)), (0.8, Some(12.0)), (0.7, Some(11.0)), (0.1, Some(9.0)), (0.2, Some(10.0))] {
            let delta = obj(vec![("virtual_sdcard", obj(vec![("progress", Value::F64(progress))]))]);
            let text = obj(vec![("jsonrpc", s("2.0")), ("method", s("notify_status_update")), ("params", Value::Arr(vec![delta, time.map_or(Value::Null, Value::F64)]))]).to_json();
            connection.lock().unwrap_or_else(|e| e.into_inner()).output.extend(frame(text.as_bytes(), 1));
        }
        for message in ["unknown time first", "unknown time last"] {
            let delta = obj(vec![("print_stats", obj(vec![("message", s(message))]))]);
            let text = obj(vec![("jsonrpc", s("2.0")), ("method", s("notify_status_update")), ("params", Value::Arr(vec![delta]))]).to_json();
            connection.lock().unwrap_or_else(|e| e.into_inner()).output.extend(frame(text.as_bytes(), 1));
        }
        // Read all deltas while the subscription is still only partly sent.
        assert!(socket.poll().unwrap().is_none());
        assert_eq!(socket.status().progress, 0.0);
        budget.set(usize::MAX);
        let status = socket.poll().unwrap().unwrap();
        assert_eq!(status.progress, 0.8, "newer deltas win by eventtime, equal times remain stable");
        assert_eq!(status.message, "unknown time last");
        sim.update(obj(vec![("virtual_sdcard", obj(vec![("progress", Value::F64(0.9))]))]));
        assert_eq!(socket.poll().unwrap().unwrap().progress, 0.9, "ready subscriptions merge deltas immediately");
        sim.step(2.0);
        assert_eq!(sim.state.lock().unwrap_or_else(|e| e.into_inner()).eventtime, 12.0);
    }
}

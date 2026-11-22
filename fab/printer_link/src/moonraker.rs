//! Moonraker HTTP and status protocol over caller-owned byte streams; no networking.
use crate::secrets::Secret;
use makepad_network::{digest::base64_encode, WebSocketMessageFormat, WebSocketMessageHeader, WebSocketParser};
use makepad_network::ParsedWebSocketMessage;
use makepad_strict_json::{obj, parse, s, Value};
use std::fmt;
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Public stream bound equivalent to the existing link's private stream bound.
pub trait ByteStream: Read + Write {}
impl<T: Read + Write> ByteStream for T {}

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    Unauthorized,
    Refused(String),
    Http(u16),
    Protocol(&'static str),
    Io,
    Cancelled,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("the printer wants an API key"),
            Self::Refused(message) => f.write_str(message),
            Self::Http(code) => write!(f, "HTTP status {code}"),
            Self::Protocol(message) => f.write_str(message),
            Self::Io => f.write_str("stream I/O failed"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(_: io::Error) -> Self { Self::Io }
}

pub fn refusal(message: &str) -> Option<&'static str> {
    message.split(|c: char| !c.is_ascii_digit() && c != '-').find_map(|code| match code {
        "531-14" => Some("the nozzle diameter does not match a used head"),
        "531-16" => Some("the printer is busy"),
        "531-17" => Some("a start parameter is wrong"),
        "531-19" => Some("a high-flow filament is on a standard nozzle"),
        "531-25" => Some("a head holds a material its nozzle does not allow"),
        _ => None,
    })
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct StartParams {
    pub bed_level: Option<bool>,
    pub flow_calibrate: Option<bool>,
    pub time_lapse_camera: Option<bool>,
    pub map_table: Vec<(u32, u32)>,
    pub flow_calibrate_extruders: Vec<u32>,
    pub line_width: Option<f64>,
    pub layer_height: Option<f64>,
    pub outer_wall_speed: Option<f64>,
    pub nozzle_diameters: Vec<f64>,
    pub nozzle_temp: Vec<f64>,
    pub filament_types: Vec<String>,
    pub filament_flow_ratio: Vec<f64>,
    pub filament_diameter: Vec<f64>,
    pub filament_max_volumetric_speed: Vec<f64>,
    pub filament_used_g: Vec<f64>,
    pub filament_used_mm: Vec<f64>,
    pub outer_wall_acceleration: Option<f64>,
    pub filament_volume_type: Vec<String>,
    pub print_sequence: Option<String>,
    pub outer_wall_speed_list: Vec<f64>,
    pub outer_wall_acceleration_list: Vec<f64>,
    pub process_flow_support: Vec<String>,
    pub slicer: Option<String>,
    pub slicer_version: Option<String>,
}
pub(super) fn safe_value(value: &str) -> Result<(), Error> {
    if value.chars().any(|c| c == '"' || c == '\'' || c == '\\' || c.is_control()) {
        return Err(Error::Protocol("invalid start value"));
    }
    Ok(())
}
pub(super) fn safe_list_value(value: &str) -> Result<(), Error> {
    safe_value(value)?;
    if value.trim().is_empty() || !value.bytes().all(|c| c.is_ascii_alphanumeric() || b"_-+. ".contains(&c)) {
        return Err(Error::Protocol("invalid start list value"));
    }
    Ok(())
}
fn file_name(name: &str) -> Result<(), Error> {
    safe_value(name)?;
    if name.contains('/') || name.contains('\\') || name.contains("..") || !name.ends_with(".gcode") || name == ".gcode" {
        return Err(Error::Protocol("invalid file name"));
    }
    Ok(())
}
fn number(n: f64) -> Result<String, Error> {
    if !n.is_finite() { return Err(Error::Protocol("non-finite start number")); }
    let mut value = if n == 0.0 { "0".into() } else { n.to_string() };
    if !value.contains('.') { value.push_str(".0"); }
    Ok(value)
}
fn list(items: impl IntoIterator<Item = String>) -> String {
    format!("[{}]", items.into_iter().collect::<Vec<_>>().join(", "))
}
fn number_list(values: &[f64]) -> Result<String, Error> {
    Ok(list(values.iter().map(|n| number(*n)).collect::<Result<Vec<_>, _>>()?))
}
fn string_list(values: &[String]) -> Result<String, Error> {
    for value in values { safe_list_value(value)?; }
    Ok(list(values.iter().map(|value| format!("'{value}'"))))
}
/// Validation can fail, so the generated command is returned in a Result.
pub fn start_line(file: &str, p: &StartParams) -> Result<String, Error> {
    file_name(file)?;
    let mut line = format!("SDCARD_PRINT_FILE_WITH_PARAMETERS FILENAME=\"{file}\"");
    let mut add = |key: &str, value: String| { line.push_str(&format!(" {key}=\"{value}\"")); };
    for (key, value) in [("BED_LEVEL", p.bed_level), ("FLOW_CALIBRATE", p.flow_calibrate), ("TIME_LAPSE_CAMERA", p.time_lapse_camera)] {
        if let Some(value) = value { add(key, u8::from(value).to_string()); }
    }
    if !p.map_table.is_empty() { add("MAP_TABLE", list(p.map_table.iter().map(|(a,b)| format!("[{a}, {b}]")))); }
    if p.flow_calibrate == Some(true) && !p.flow_calibrate_extruders.is_empty() {
        add("FLOW_CALIBRATE_EXTRUDERS", list(p.flow_calibrate_extruders.iter().map(u32::to_string)));
    }
    for (key, value) in [("LINE_WIDTH", p.line_width), ("LAYER_HEIGHT", p.layer_height), ("OUTER_WALL_SPEED", p.outer_wall_speed)] {
        if let Some(value) = value { add(key, number(value)?); }
    }
    for (key, values) in [("NOZZLE_DIAMETER_LIST", &p.nozzle_diameters), ("NOZZLE_TEMP", &p.nozzle_temp)] {
        if !values.is_empty() { add(key, number_list(values)?); }
    }
    if !p.filament_types.is_empty() { add("FILAMENT_TYPE", string_list(&p.filament_types)?); }
    for (key, values) in [
        ("FILAMENT_FLOW_RATIO", &p.filament_flow_ratio), ("FILAMENT_DIAMETER", &p.filament_diameter),
        ("FILAMENT_MAX_VOLUMETRIC_SPEED", &p.filament_max_volumetric_speed),
        ("FILAMENT_USED_G", &p.filament_used_g), ("FILAMENT_USED_MM", &p.filament_used_mm),
    ] {
        if !values.is_empty() { add(key, number_list(values)?); }
    }
    if let Some(value) = p.outer_wall_acceleration { add("OUTER_WALL_ACCELERATION", number(value)?); }
    if !p.filament_volume_type.is_empty() { add("FILAMENT_VOLUME_TYPE", string_list(&p.filament_volume_type)?); }
    if let Some(value) = &p.print_sequence { safe_value(value)?; add("PRINT_SEQUENCE", value.clone()); }
    for (key, values) in [("OUTER_WALL_SPEED_LIST", &p.outer_wall_speed_list), ("OUTER_WALL_ACCELERATION_LIST", &p.outer_wall_acceleration_list)] {
        if !values.is_empty() { add(key, number_list(values)?); }
    }
    if !p.process_flow_support.is_empty() { add("PROCESS_FLOW_SUPPORT", string_list(&p.process_flow_support)?); }
    for (key, value) in [("SLICER", &p.slicer), ("SLICER_VERSION", &p.slicer_version)] {
        if let Some(value) = value { safe_value(value)?; add(key, value.clone()); }
    }
    Ok(line)
}

/// Response caps are configurable; upload payloads are not subject to a size cap.
#[derive(Clone, Copy, Debug)]
pub struct Limits { pub header: usize, pub body: usize }
impl Default for Limits {
    fn default() -> Self { Self { header: 32 * 1024, body: 16 * 1024 * 1024 } }
}
struct Head { code: u16, fields: Vec<(String, String)> }
impl Head {
    fn get(&self, name: &str) -> Option<&str> {
        self.fields.iter().find(|(k,_)| k.eq_ignore_ascii_case(name)).map(|(_,v)| v.as_str())
    }
}
fn retryable(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted)
}
#[derive(Default)]
struct Waiting<'a> { stop: Option<&'a AtomicBool>, cancel: Option<&'a AtomicBool> }
impl Waiting<'_> {
    fn check(&self) -> Result<(), Error> {
        if self.stop.into_iter().chain(self.cancel).any(|flag| flag.load(Ordering::Acquire)) {
            Err(Error::Cancelled)
        } else { Ok(()) }
    }
}
fn send(stream: &mut impl Write, mut bytes: &[u8], waiting: &Waiting<'_>) -> Result<(), Error> {
    while !bytes.is_empty() {
        waiting.check()?;
        match stream.write(bytes) {
            Ok(0) => return Err(Error::Io),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if retryable(&e) => { waiting.check()?; std::thread::park_timeout(std::time::Duration::from_millis(1)); }
            Err(_) => return Err(Error::Io),
        }
    }
    waiting.check()
}
fn flush(stream: &mut impl Write, waiting: &Waiting<'_>) -> Result<(), Error> {
    loop {
        waiting.check()?;
        match stream.flush() {
            Ok(()) => return Ok(()),
            Err(e) if retryable(&e) => { waiting.check()?; std::thread::park_timeout(std::time::Duration::from_millis(1)); }
            Err(_) => return Err(Error::Io),
        }
    }
}
#[derive(Default)]
struct Input { bytes: Vec<u8>, position: usize }
impl Input {
    fn fill(&mut self, stream: &mut impl Read, waiting: &Waiting<'_>) -> Result<(), Error> {
        if self.position < self.bytes.len() { return waiting.check(); }
        self.bytes.clear();
        self.position = 0;
        let mut buffer = [0; 8192];
        loop {
            waiting.check()?;
            match stream.read(&mut buffer) {
                Ok(0) => return Err(Error::Io),
                Ok(n) => { self.bytes.extend_from_slice(&buffer[..n]); return waiting.check(); }
                Err(e) if retryable(&e) => { waiting.check()?; std::thread::park_timeout(std::time::Duration::from_millis(1)); }
                Err(_) => return Err(Error::Io),
            }
        }
    }
    fn line(&mut self, stream: &mut impl Read, left: &mut usize, waiting: &Waiting<'_>) -> Result<String, Error> {
        let mut bytes = Vec::new();
        loop {
            if *left == 0 { return Err(Error::Protocol("HTTP headers exceed cap")); }
            self.fill(stream, waiting)?;
            bytes.push(self.bytes[self.position]);
            self.position += 1;
            *left -= 1;
            if bytes.ends_with(b"\r\n") {
                bytes.truncate(bytes.len() - 2);
                return String::from_utf8(bytes).map_err(|_| Error::Protocol("invalid HTTP header"));
            }
        }
    }
    fn head(&mut self, stream: &mut impl Read, cap: usize, waiting: &Waiting<'_>) -> Result<Head, Error> {
        let mut left = cap;
        let line = self.line(stream, &mut left, waiting)?;
        let mut words = line.split_whitespace();
        if words.next() != Some("HTTP/1.1") { return Err(Error::Protocol("invalid HTTP status")); }
        let code = words.next().filter(|s| s.len() == 3).and_then(|s| s.parse::<u16>().ok()).filter(|n| (100..600).contains(n)).ok_or(Error::Protocol("invalid HTTP status"))?;
        let mut fields = Vec::new();
        loop {
            let line = self.line(stream, &mut left, waiting)?;
            if line.is_empty() { break; }
            let (k,v) = line.split_once(':').ok_or(Error::Protocol("invalid HTTP header"))?;
            if k.is_empty() || !k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') { return Err(Error::Protocol("invalid HTTP header")); }
            if fields.iter().any(|(old, _): &(String,String)| old.eq_ignore_ascii_case(k)) { return Err(Error::Protocol("duplicate HTTP header")); }
            fields.push((k.into(), v.trim().into()));
        }
        Ok(Head { code, fields })
    }
    fn append(&mut self, stream: &mut impl Read, body: &mut Vec<u8>, mut size: usize, waiting: &Waiting<'_>) -> Result<(), Error> {
        while size > 0 {
            self.fill(stream, waiting)?;
            let count = size.min(self.bytes.len() - self.position);
            body.try_reserve(count).map_err(|_| Error::Protocol("HTTP body exceeds cap"))?;
            body.extend_from_slice(&self.bytes[self.position..self.position + count]);
            self.position += count;
            size -= count;
        }
        waiting.check()
    }
    fn body(&mut self, stream: &mut impl Read, head: &Head, limits: Limits, waiting: &Waiting<'_>) -> Result<Vec<u8>, Error> {
        let mut body = Vec::new();
        if let Some(encoding) = head.get("Transfer-Encoding") {
            if !encoding.eq_ignore_ascii_case("chunked") || head.get("Content-Length").is_some() { return Err(Error::Protocol("invalid HTTP framing")); }
            loop {
                let mut left = limits.header;
                let line = self.line(stream, &mut left, waiting)?;
                let size = usize::from_str_radix(line.split(';').next().unwrap_or("").trim(), 16).map_err(|_| Error::Protocol("invalid chunk length"))?;
                if size == 0 {
                    while !self.line(stream, &mut left, waiting)?.is_empty() {}
                    return Ok(body);
                }
                if size > limits.body.saturating_sub(body.len()) { return Err(Error::Protocol("HTTP body exceeds cap")); }
                self.append(stream, &mut body, size, waiting)?;
                let mut end = Vec::new();
                self.append(stream, &mut end, 2, waiting)?;
                if end != b"\r\n" { return Err(Error::Protocol("invalid chunk ending")); }
            }
        }
        if head.code == 204 { return Ok(body); }
        let size: usize = head.get("Content-Length").ok_or(Error::Protocol("missing HTTP length"))?.parse().map_err(|_| Error::Protocol("invalid HTTP length"))?;
        if size > limits.body { return Err(Error::Protocol("HTTP body exceeds cap")); }
        self.append(stream, &mut body, size, waiting)?;
        Ok(body)
    }
}
fn random_bytes() -> [u8;16] {
    let mut bytes = [0;16];
    for chunk in bytes.chunks_exact_mut(8) { chunk.copy_from_slice(&RandomState::new().build_hasher().finish().to_be_bytes()); }
    bytes
}
fn boundary_with(file: &[u8], mut next: impl FnMut() -> String) -> String {
    loop {
        let boundary = next();
        if !boundary.is_empty() && !file.windows(boundary.len()).any(|w| w == boundary.as_bytes()) { return boundary; }
    }
}
fn boundary(file: &[u8]) -> String {
    boundary_with(file, || {
        let mut text = String::from("fab-");
        for b in random_bytes() { use fmt::Write; let _ = write!(text, "{b:02x}"); }
        text
    })
}
fn validate_host_key(host: &str, key: Option<&Secret>) -> Result<(), Error> {
    if host.is_empty() || host.bytes().any(|c| !c.is_ascii_graphic() || matches!(c, b'/' | b'@' | b'\\')) { return Err(Error::Protocol("invalid host header")); }
    if key.is_some_and(|key| key.bytes().is_empty() || key.bytes().iter().any(|c| !c.is_ascii_graphic())) { return Err(Error::Protocol("invalid API key")); }
    Ok(())
}
fn write_key(bytes: &mut Vec<u8>, key: Option<&Secret>) {
    if let Some(key) = key { bytes.extend_from_slice(b"X-Api-Key: "); bytes.extend_from_slice(key.bytes()); bytes.extend_from_slice(b"\r\n"); }
}
fn redact(value: &mut Value, key: &Secret) {
    match value {
        Value::Str(text) => *text = key.redact(text),
        Value::Arr(values) => { for value in values { redact(value, key); } }
        Value::Obj(fields) => { for (name, value) in fields { *name = key.redact(name); redact(value, key); } }
        _ => {}
    }
}
fn remote_error(value: &Value) -> Error {
    Error::Refused(value.get("message").and_then(Value::as_str).unwrap_or("request refused").to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo { pub klippy_state: String, pub version: String }

/// HTTP exchanges want a blocking stream; a read timeout only makes the client look again.
/// Temporary I/O pauses retain the buffered response and request position, without a time limit.
pub struct Client<S> {
    stream: Option<S>,
    host: String,
    key: Option<Secret>,
    limits: Limits,
    input: Input,
    stop: Option<Arc<AtomicBool>>,
}
impl<S: ByteStream> Client<S> {
    pub fn new(stream: S, host: &str, key: Option<Secret>) -> Result<Self, Error> {
        validate_host_key(host, key.as_ref())?;
        Ok(Self { stream: Some(stream), host: host.into(), key, limits: Limits::default(), input: Input::default(), stop: None })
    }
    pub fn set_limits(&mut self, limits: Limits) { self.limits = limits; }
    pub fn set_stop(&mut self, stop: Arc<AtomicBool>) { self.stop = Some(stop); }
    fn send(&mut self, bytes: &[u8], cancel: Option<&AtomicBool>) -> Result<(), Error> {
        let result = send(self.stream.as_mut().ok_or(Error::Protocol("stream is closed"))?, bytes, &Waiting { stop: self.stop.as_deref(), cancel });
        if result.is_err() { self.stream.take(); }
        result
    }
    fn header(&mut self, method: &str, path: &str, content_type: &str, length: usize, cancel: Option<&AtomicBool>) -> Result<(), Error> {
        let mut bytes = format!("{method} {path} HTTP/1.1\r\nHost: {}\r\nConnection: keep-alive\r\nContent-Length: {length}\r\nContent-Type: {content_type}\r\n", self.host).into_bytes();
        write_key(&mut bytes, self.key.as_ref());
        bytes.extend_from_slice(b"\r\n");
        self.send(Secret::new(bytes).bytes(), cancel)
    }
    fn response(&mut self, cancel: Option<&AtomicBool>) -> Result<Value, Error> {
        let result = (|| {
            let stream = self.stream.as_mut().ok_or(Error::Protocol("stream is closed"))?;
            let waiting = Waiting { stop: self.stop.as_deref(), cancel };
            flush(stream, &waiting)?;
            let head = self.input.head(stream, self.limits.header, &waiting)?;
            if head.code == 401 { return Err(Error::Unauthorized); }
            let body = self.input.body(stream, &head, self.limits, &waiting)?;
            if !matches!(head.code, 200 | 201 | 204 | 400 | 403) { return Err(Error::Http(head.code)); }
            let mut value = if body.is_empty() && head.code != 400 { Value::Null } else {
                parse(&body).map_err(|_| Error::Protocol(if head.code == 400 { "invalid error JSON" } else { "invalid response JSON" }))?
            };
            if let Some(key) = &self.key { redact(&mut value, key); }
            if head.code == 403 {
                let error = remote_error(value.get("error").unwrap_or(&Value::Null));
                return Err(if matches!(&error, Error::Refused(message) if message == "File currently in use") { error } else { Error::Unauthorized });
            }
            if head.code == 400 { return Err(remote_error(value.get("error").unwrap_or(&Value::Null))); }
            Ok(value)
        })();
        if matches!(result, Err(Error::Io | Error::Protocol(_) | Error::Unauthorized | Error::Cancelled)) { self.stream.take(); }
        result
    }
    fn request(&mut self, method: &str, path: &str, body: &[u8]) -> Result<Value, Error> {
        self.header(method, path, "application/json", body.len(), None)?;
        self.send(body, None)?;
        self.response(None)
    }
    pub fn server_info(&mut self) -> Result<ServerInfo, Error> {
        let value = self.request("GET", "/server/info", &[])?;
        let result = value.get("result").ok_or(Error::Protocol("missing result"))?;
        let state = result.get("klippy_state").and_then(Value::as_str).ok_or(Error::Protocol("missing state"))?;
        if !matches!(state, "ready" | "startup" | "shutdown" | "error" | "disconnected") { return Err(Error::Protocol("invalid server state")); }
        Ok(ServerInfo { klippy_state: state.into(), version: result.get("moonraker_version").and_then(Value::as_str).ok_or(Error::Protocol("missing version"))?.into() })
    }
    pub fn upload(&mut self, name: &str, file: &[u8], cancel: &AtomicBool, progress: impl FnMut(usize, usize)) -> Result<String, Error> {
        self.upload_inner(name, file, cancel, progress, false)
    }
    /// Keep the receipt once fully sent, so a stopped job can clean up its stored file.
    pub(crate) fn upload_with_receipt(&mut self, name: &str, file: &[u8], cancel: &AtomicBool, progress: impl FnMut(usize, usize)) -> Result<String, Error> {
        self.upload_inner(name, file, cancel, progress, true)
    }
    fn upload_inner(&mut self, name: &str, file: &[u8], cancel: &AtomicBool, mut progress: impl FnMut(usize, usize), keep_receipt: bool) -> Result<String, Error> {
        file_name(name)?;
        if cancel.load(Ordering::Acquire) { self.stream.take(); return Err(Error::Cancelled); }
        let boundary = boundary(file);
        let prefix = format!("--{boundary}\r\nContent-Disposition: form-data; name=\"root\"\r\n\r\ngcodes\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"print\"\r\n\r\nfalse\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n");
        let suffix = format!("\r\n--{boundary}--\r\n");
        let length = file.len().checked_add(prefix.len()).and_then(|n| n.checked_add(suffix.len())).ok_or(Error::Protocol("upload length overflow"))?;
        let result = (|| {
            self.header("POST", "/server/files/upload", &format!("multipart/form-data; boundary={boundary}"), length, Some(cancel))?;
            self.send(prefix.as_bytes(), Some(cancel))?;
            let mut sent = 0;
            progress(0, file.len());
            for slice in file.chunks(16 * 1024) {
                if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
                self.send(slice, Some(cancel))?;
                sent += slice.len();
                progress(sent, file.len());
            }
            if cancel.load(Ordering::Acquire) { return Err(Error::Cancelled); }
            self.send(suffix.as_bytes(), Some(cancel))?;
            // Once the entire file was sent, keep its reply for cancellation cleanup.
            let value = self.response(if keep_receipt { None } else { Some(cancel) })?;
            value.get("result").and_then(|v| v.get("item")).and_then(|v| v.get("path")).and_then(Value::as_str).map(str::to_string).ok_or(Error::Protocol("missing stored path"))
        })();
        if result.is_err() { self.stream.take(); }
        result
    }
    pub fn script(&mut self, line: &str) -> Result<(), Error> {
        self.request("POST", "/printer/gcode/script", obj(vec![("script", s(line))]).to_json().as_bytes())?;
        Ok(())
    }
    /// Lists the files in the G-code storage root.
    pub fn list(&mut self) -> Result<Vec<crate::ftps::Entry>, Error> {
        let value = self.request("GET", "/server/files/list?root=gcodes", &[])?;
        value.get("result").and_then(Value::as_arr).ok_or(Error::Protocol("missing file list"))?.iter().map(|file| {
            Ok(crate::ftps::Entry {
                name: file.get("path").and_then(Value::as_str).ok_or(Error::Protocol("missing file name"))?.to_string(),
                size: Some(file.get("size").and_then(Value::as_u64).ok_or(Error::Protocol("missing file size"))?),
                dir: false,
            })
        }).collect()
    }
    /// Deletes a file in the G-code root; names obey the start command's restrictions.
    pub fn delete(&mut self, file: &str) -> Result<(), Error> {
        file_name(file)?;
        let mut path = String::from("/server/files/gcodes/");
        for byte in file.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                path.push(char::from(byte));
            } else {
                use fmt::Write;
                let _ = write!(path, "%{byte:02X}");
            }
        }
        match self.request("DELETE", &path, &[]) {
            Ok(_) => Ok(()),
            Err(Error::Refused(message)) if message == "Invalid file path" => Ok(()),
            Err(error) => Err(error),
        }
    }
    pub fn pause(&mut self) -> Result<(), Error> { self.request("POST", "/printer/print/pause", &[]).map(|_| ()) }
    pub fn resume(&mut self) -> Result<(), Error> { self.request("POST", "/printer/print/resume", &[]).map(|_| ()) }
    pub fn cancel(&mut self) -> Result<(), Error> { self.request("POST", "/printer/print/cancel", &[]).map(|_| ()) }
    pub fn query(&mut self, objects: &[&str]) -> Result<Value, Error> {
        if objects.iter().any(|o| o.is_empty() || !o.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')) { return Err(Error::Protocol("invalid object name")); }
        let value = self.request("GET", &format!("/printer/objects/query?{}", objects.join("&")), &[])?;
        value.get("result").and_then(|v| v.get("status")).cloned().ok_or(Error::Protocol("missing status"))
    }
}

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Heater { pub temperature: f64, pub target: f64 }
#[derive(Clone, Debug, PartialEq)]
pub struct Status {
    pub state: String,
    pub filename: String,
    pub print_duration: f64,
    pub message: String,
    pub current_layer: u64,
    pub total_layer: u64,
    pub progress: f64,
    pub is_active: bool,
    pub file_path: String,
    pub firmware: String,
    pub firmware_message: String,
    pub bed: Heater,
    pub extruders: Vec<Heater>,
    pub active_extruder: String,
    pub objects: Vec<String>,
    pub excluded_objects: Vec<String>,
    pub current_object: Option<String>,
    pub print_task_config: Value,
    pub filament_detect: Value,
}
impl Status {
    /// Nothing reported yet, for a printer with `heads` heads.
    pub fn new(heads: usize) -> Self {
        Self { state: String::new(), filename: String::new(), print_duration: 0.0, message: String::new(), current_layer: 0, total_layer: 0, progress: 0.0, is_active: false, file_path: String::new(), firmware: String::new(), firmware_message: String::new(), bed: Heater::default(), extruders: vec![Heater::default(); heads], active_extruder: String::new(), objects: Vec::new(), excluded_objects: Vec::new(), current_object: None, print_task_config: obj(vec![]), filament_detect: obj(vec![]) }
    }
}
/// Head `h`'s heater object: "extruder", then "extruder1", "extruder2", …
pub fn heater_name(h: usize) -> String {
    if h == 0 { "extruder".into() } else { format!("extruder{h}") }
}
fn numeric(v: &Value) -> Option<f64> { match v { Value::F64(n) => Some(*n), Value::Int(n) => Some(*n as f64), _ => None } }
fn text_field(v: &Value, key: &str, out: &mut String) { if let Some(s) = v.get(key).and_then(Value::as_str) { *out = s.into(); } }
fn float_field(v: &Value, key: &str, out: &mut f64) { if let Some(n) = v.get(key).and_then(numeric) { *out = n; } }
fn merge(target: &mut Value, delta: &Value) {
    if let (Value::Obj(target), Value::Obj(delta)) = (&mut *target, delta) {
        for (key, value) in delta {
            if let Some((_, old)) = target.iter_mut().find(|(k,_)| k == key) { merge(old, value); }
            else { target.push((key.clone(), value.clone())); }
        }
    } else { *target = delta.clone(); }
}
impl Status {
    pub fn merge(&mut self, delta: &Value) -> Result<(), Error> {
        if !matches!(delta, Value::Obj(_)) { return Err(Error::Protocol("invalid status")); }
        if let Some(v) = delta.get("print_stats") {
            text_field(v, "state", &mut self.state); text_field(v, "filename", &mut self.filename); text_field(v, "message", &mut self.message);
            float_field(v, "print_duration", &mut self.print_duration);
            if let Some(v) = v.get("info") {
                if let Some(n) = v.get("current_layer").and_then(Value::as_u64) { self.current_layer = n; }
                if let Some(n) = v.get("total_layer").and_then(Value::as_u64) { self.total_layer = n; }
            }
        }
        if let Some(v) = delta.get("webhooks") { text_field(v, "state", &mut self.firmware); text_field(v, "state_message", &mut self.firmware_message); }
        if let Some(v) = delta.get("virtual_sdcard") {
            if let Some(active) = v.get("is_active").and_then(Value::as_bool) { self.is_active = active; }
            text_field(v, "file_path", &mut self.file_path);
            float_field(v, "progress", &mut self.progress); self.progress = self.progress.clamp(0.0, 1.0); }
        for (h, heater) in std::iter::once((None, &mut self.bed)).chain(self.extruders.iter_mut().enumerate().map(|(h, e)| (Some(h), e))) {
            let key = h.map_or_else(|| "heater_bed".to_string(), heater_name);
            if let Some(v) = delta.get(&key) { float_field(v, "temperature", &mut heater.temperature); float_field(v, "target", &mut heater.target); }
        }
        if let Some(v) = delta.get("toolhead") { text_field(v, "extruder", &mut self.active_extruder); }
        if let Some(v) = delta.get("exclude_object") {
            if let Some(a) = v.get("objects").and_then(Value::as_arr) { self.objects = a.iter().filter_map(|v| v.get("name").and_then(Value::as_str).map(str::to_string)).collect(); }
            if let Some(a) = v.get("excluded_objects").and_then(Value::as_arr) { self.excluded_objects = a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect(); }
            if let Some(v) = v.get("current_object") { self.current_object = v.as_str().map(str::to_string); }
        }
        if let Some(v) = delta.get("print_task_config") { merge(&mut self.print_task_config, v); }
        if let Some(v) = delta.get("filament_detect") { merge(&mut self.filament_detect, v); }
        Ok(())
    }
}

/// The objects the status subscribes to, besides each head's heater.
const OBJECTS: &[&str] = &["webhooks", "print_stats", "virtual_sdcard", "heater_bed", "toolhead", "exclude_object", "print_task_config", "filament_detect"];
/// Every object the status of a printer with `heads` heads subscribes to.
pub fn objects(heads: usize) -> Vec<String> {
    OBJECTS.iter().map(|o| o.to_string()).chain((0..heads).map(heater_name)).collect()
}
fn frame(data: &[u8], opcode: u8) -> Vec<u8> {
    let header = WebSocketMessageHeader::from_len(data.len(), WebSocketMessageFormat::Text, true);
    let mut bytes = WebSocketParser::build_message(header, data);
    bytes[0] = 0x80 | opcode;
    bytes
}
#[derive(PartialEq)]
enum Phase { Identify, Subscribe, Ready, Suspended, Closed }
pub struct StatusSocket<S> {
    stream: S,
    parser: WebSocketParser,
    status: Status,
    /// The heads whose heaters it subscribes to: the printer profile's.
    heads: usize,
    key: Option<Secret>,
    phase: Phase,
    id: i64,
    incoming: Vec<u8>,
    outgoing: Vec<u8>,
    flush_pending: bool,
    deltas: Vec<(Value, Option<f64>)>,
    fragment: Vec<u8>,
    fragmented: bool,
}
impl<S: ByteStream> StatusSocket<S> {
    /// Sends the HTTP upgrade and identification. Poll completes identification and subscription.
    pub fn connect(stream: S, host: &str, key: Option<Secret>, heads: usize) -> Result<Self, Error> {
        Self::connect_stoppable(stream, host, key, heads, None)
    }
    pub(crate) fn connect_stoppable(mut stream: S, host: &str, key: Option<Secret>, heads: usize, stop: Option<&AtomicBool>) -> Result<Self, Error> {
        validate_host_key(host, key.as_ref())?;
        let nonce = base64_encode(&random_bytes());
        let mut bytes = format!("GET /websocket HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: {nonce}\r\n").into_bytes();
        write_key(&mut bytes, key.as_ref());
        bytes.extend_from_slice(b"\r\n");
        let waiting = Waiting { stop, cancel: None };
        send(&mut stream, Secret::new(bytes).bytes(), &waiting)?;
        flush(&mut stream, &waiting)?;
        let mut input = Input::default();
        let head = input.head(&mut stream, Limits::default().header, &waiting)?;
        if matches!(head.code, 401 | 403) { return Err(Error::Unauthorized); }
        if head.code != 101 { return Err(Error::Http(head.code)); }
        let expected = WebSocketParser::create_upgrade_response(&nonce);
        let expected = Input::default().head(&mut io::Cursor::new(expected.as_bytes()), Limits::default().header, &waiting)?;
        if head.get("Sec-WebSocket-Accept") != expected.get("Sec-WebSocket-Accept") || !head.get("Upgrade").is_some_and(|v| v.eq_ignore_ascii_case("websocket")) || !head.get("Connection").is_some_and(|v| v.split(',').any(|v| v.trim().eq_ignore_ascii_case("upgrade"))) { return Err(Error::Protocol("invalid websocket upgrade")); }
        let mut socket = Self { stream, parser: WebSocketParser::new(), status: Status::new(heads), heads, key, phase: Phase::Identify, id: 0, incoming: input.bytes[input.position..].to_vec(), outgoing: Vec::new(), flush_pending: false, deltas: Vec::new(), fragment: Vec::new(), fragmented: false };
        let mut params = obj(vec![("client_name", s("Fab")), ("version", s(env!("CARGO_PKG_VERSION"))), ("type", s("desktop")), ("url", s(""))]);
        if let (Some(key), Value::Obj(fields)) = (&socket.key, &mut params) {
            fields.push(("api_key".into(), s(std::str::from_utf8(key.bytes()).map_err(|_| Error::Protocol("invalid API key"))?)));
        }
        socket.rpc("server.connection.identify", params, Phase::Identify)?;
        Ok(socket)
    }
    fn rpc(&mut self, method: &str, params: Value, phase: Phase) -> Result<(), Error> {
        let id = self.id + 1;
        let text = obj(vec![("jsonrpc", s("2.0")), ("id", Value::Int(id)), ("method", s(method)), ("params", params)]).to_json();
        self.outgoing.extend(frame(text.as_bytes(), 1));
        self.id = id;
        self.phase = phase;
        self.write_pending()
    }
    fn write_pending(&mut self) -> Result<(), Error> {
        let result = (|| {
            let mut sent = 0;
            while sent < self.outgoing.len() {
                match self.stream.write(&self.outgoing[sent..]) {
                    Ok(0) => return Err(Error::Io),
                    Ok(n) => { sent += n; self.flush_pending = true; }
                    Err(e) if retryable(&e) => break,
                    Err(_) => return Err(Error::Io),
                }
            }
            self.outgoing.drain(..sent);
            if !self.flush_pending { return Ok(()); }
            match self.stream.flush() {
                Ok(()) => { self.flush_pending = false; Ok(()) },
                Err(e) if retryable(&e) => Ok(()),
                Err(_) => Err(Error::Io),
            }
        })();
        if result.is_err() { self.phase = Phase::Closed; }
        result
    }
    pub fn status(&self) -> &Status { &self.status }
    pub fn is_closed(&self) -> bool { self.phase == Phase::Closed }
    pub(crate) fn fresh(&self) -> bool { self.phase == Phase::Ready }
    pub(crate) fn resubscribe(&mut self) -> Result<(), Error> {
        self.status = Status::new(self.heads);
        self.deltas.clear();
        self.rpc("printer.objects.subscribe", obj(vec![("objects", Value::Obj(objects(self.heads).into_iter().map(|name| (name, Value::Null)).collect()))]), Phase::Subscribe)
    }
    /// Watch `heads` heads: a subscription in place is made again; one still to come (the
    /// identification or the firmware's return asks for it) takes the new count.
    pub(crate) fn set_heads(&mut self, heads: usize) -> Result<(), Error> {
        if heads == self.heads { return Ok(()); }
        self.heads = heads;
        match self.phase {
            Phase::Subscribe | Phase::Ready => self.resubscribe(),
            Phase::Identify | Phase::Suspended | Phase::Closed => Ok(()),
        }
    }
    fn message(&mut self, text: &[u8]) -> Result<bool, Error> {
        let mut value = parse(text).map_err(|_| Error::Protocol("invalid RPC JSON"))?;
        if let Some(key) = &self.key { redact(&mut value, key); }
        if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0") { return Err(Error::Protocol("invalid RPC version")); }
        if value.get("id").is_some() {
            if value.get("id").and_then(Value::as_i64) != Some(self.id) { return Ok(false); }
            if let Some(error) = value.get("error") { return Err(remote_error(error)); }
            match self.phase {
                Phase::Identify => {
                    if value.get("result").is_none() { return Err(Error::Protocol("missing RPC result")); }
                    self.resubscribe()?;
                }
                Phase::Subscribe => {
                    let status = value.get("result").and_then(|v| v.get("status")).ok_or(Error::Protocol("missing subscription status"))?;
                    self.status = Status::new(self.heads);
                    self.status.merge(status)?;
                    let snapshot_time = value.get("result").and_then(|v| v.get("eventtime")).and_then(numeric);
                    let mut deltas = std::mem::take(&mut self.deltas);
                    deltas.sort_by(|a, b| a.1.unwrap_or(f64::INFINITY).total_cmp(&b.1.unwrap_or(f64::INFINITY)));
                    for (delta, time) in deltas {
                        if time.zip(snapshot_time).is_none_or(|(time, snapshot)| time > snapshot) {
                            self.status.merge(&delta)?;
                        }
                    }
                    self.phase = Phase::Ready;
                    return Ok(true);
                }
                _ => {}
            }
        } else if let Some(method @ ("notify_klippy_ready" | "notify_klippy_shutdown" | "notify_klippy_disconnected")) = value.get("method").and_then(Value::as_str) {
            if method == "notify_klippy_ready" {
                self.resubscribe()?;
            } else {
                self.id += 1;
                self.phase = Phase::Suspended;
                self.deltas.clear();
                self.status.firmware = if method == "notify_klippy_shutdown" { "shutdown" } else { "disconnected" }.into();
            }
            return Ok(true);
        } else if value.get("method").and_then(Value::as_str) == Some("notify_status_update") {
            let delta = value.get("params").and_then(Value::as_arr).and_then(|a| a.first()).ok_or(Error::Protocol("missing status delta"))?;
            if self.phase == Phase::Subscribe {
                let time = value.get("params").and_then(Value::as_arr).and_then(|a| a.get(1)).and_then(numeric);
                self.deltas.push((delta.clone(), time));
                return Ok(false);
            }
            if self.phase == Phase::Suspended {
                let before = self.status.firmware_message.clone();
                if let Some(webhooks) = delta.get("webhooks") { text_field(webhooks, "state_message", &mut self.status.firmware_message); }
                return Ok(before != self.status.firmware_message);
            }
            if self.phase != Phase::Ready { return Ok(false); }
            let before = self.status.clone();
            self.status.merge(delta)?;
            return Ok(before != self.status);
        }
        Ok(false)
    }
    /// Nonblocking streams return None on WouldBlock, retaining partial frames and RPC state.
    pub fn poll(&mut self) -> Result<Option<&Status>, Error> {
        if self.is_closed() { return Ok(None); }
        self.write_pending()?;
        let mut changed = false;
        loop {
            while let Some((length, start, opcode, fin)) = frame_length(&self.incoming)? {
                if self.incoming.len() < length { break; }
                let bytes: Vec<_> = self.incoming.drain(..length).collect();
                if matches!(opcode, 0 | 1) {
                    if (opcode == 0) != self.fragmented { return Err(Error::Protocol("invalid fragmented message")); }
                    if self.fragment.len() + length - start > Limits::default().body { return Err(Error::Protocol("websocket message exceeds cap")); }
                    self.fragment.extend_from_slice(&bytes[start..]);
                    self.fragmented = !fin;
                    if fin {
                        let text = std::mem::take(&mut self.fragment);
                        changed |= self.message(&text)?;
                    }
                } else {
                    let mut ping = None;
                    let mut closed = false;
                    let mut invalid = false;
                    self.parser.parse(&bytes, |event| match event {
                        Ok(ParsedWebSocketMessage::Ping(data)) => ping = Some(data.to_vec()),
                        Ok(ParsedWebSocketMessage::Pong(_)) => {},
                        Ok(ParsedWebSocketMessage::Close) => closed = true,
                        _ => invalid = true,
                    });
                    if invalid { return Err(Error::Protocol("invalid websocket frame")); }
                    if let Some(data) = ping { self.outgoing.extend(frame(&data, 10)); self.write_pending()?; }
                    if closed { self.phase = Phase::Closed; return Ok(changed.then_some(&self.status)); }
                }
            }
            let mut buffer = [0;8192];
            match self.stream.read(&mut buffer) {
                Ok(0) => { self.phase = Phase::Closed; return Ok(changed.then_some(&self.status)); }
                Ok(n) => self.incoming.extend_from_slice(&buffer[..n]),
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Ok(changed.then_some(&self.status)),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => { self.phase = Phase::Closed; return Err(Error::Io); }
            }
        }
    }
}
fn frame_length(bytes: &[u8]) -> Result<Option<(usize, usize, u8, bool)>, Error> {
    if bytes.len() < 2 { return Ok(None); }
    if bytes[0] & 0x70 != 0 || bytes[1] & 0x80 != 0 { return Err(Error::Protocol("invalid server frame flags")); }
    let opcode = bytes[0] & 15;
    let fin = bytes[0] & 128 != 0;
    let tag = bytes[1] & 127;
    let (size, start) = match tag {
        126 if bytes.len() < 4 => return Ok(None),
        126 => (u16::from_be_bytes([bytes[2],bytes[3]]) as u64, 4),
        127 if bytes.len() < 10 => return Ok(None),
        127 => (u64::from_be_bytes(bytes[2..10].try_into().map_err(|_| Error::Protocol("invalid frame length"))?), 10),
        n => (n as u64, 2),
    };
    if size > Limits::default().body as u64 { return Err(Error::Protocol("websocket frame exceeds cap")); }
    if opcode >= 8 && (!fin || size > 125) { return Err(Error::Protocol("invalid control frame")); }
    if !matches!(opcode, 0 | 1 | 8 | 9 | 10) { return Err(Error::Protocol("unsupported websocket opcode")); }
    Ok(Some((start + size as usize, start, opcode, fin)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moonraker_sim::{SimMoonraker, SIM_HEADS};
    use std::io::Cursor;

    /// A six-head printer's status subscribes to six heaters and reads the sixth; told it has
    /// four, the socket subscribes again to four.
    #[test]
    fn status_watches_every_head_the_profile_has() {
        let sim = SimMoonraker::with_heads(None, 6);
        let mut socket = StatusSocket::connect(sim.stream(), "memory", None, 6).unwrap();
        let status = socket.poll().unwrap().unwrap();
        assert_eq!(status.extruders.len(), 6);
        assert_eq!(status.extruders[5].temperature, 20.0);
        let heaters = |sim: &SimMoonraker| sim.subscribed().into_iter().filter(|o| o.starts_with("extruder")).collect::<Vec<_>>();
        assert_eq!(heaters(&sim), ["extruder", "extruder1", "extruder2", "extruder3", "extruder4", "extruder5"]);
        let mut status = Status::new(6);
        status.merge(&parse(br#"{"extruder5":{"temperature":201,"target":215}}"#).unwrap()).unwrap();
        assert_eq!(status.extruders[5], Heater { temperature: 201.0, target: 215.0 });
        socket.set_heads(4).unwrap();
        assert!((0..100).any(|_| socket.poll().unwrap().is_some_and(|s| s.extruders.len() == 4)));
        assert_eq!(heaters(&sim), ["extruder", "extruder1", "extruder2", "extruder3"]);
    }

    #[test]
    fn start_values_are_canonical_and_checked() {
        let mut params = StartParams {
            map_table: vec![(0,0),(1,1),(2,2),(3,3)], bed_level: Some(true),
            nozzle_diameters: vec![0.4;4], filament_types: vec!["PLA".into();4],
            filament_used_g: vec![1.2,0.0,0.0,3.5], ..Default::default()
        };
        assert_eq!(start_line("cube.gcode", &params).unwrap(), "SDCARD_PRINT_FILE_WITH_PARAMETERS FILENAME=\"cube.gcode\" BED_LEVEL=\"1\" MAP_TABLE=\"[[0, 0], [1, 1], [2, 2], [3, 3]]\" NOZZLE_DIAMETER_LIST=\"[0.4, 0.4, 0.4, 0.4]\" FILAMENT_TYPE=\"['PLA', 'PLA', 'PLA', 'PLA']\" FILAMENT_USED_G=\"[1.2, 0.0, 0.0, 3.5]\"");
        for bad in ["a\"b", "a'b", "a\nb", "a\u{7f}b", "PLA\\", "PLA, PETG", "[PLA]", "PLA;", "Plá", ""] {
            params.filament_types = vec![bad.into()];
            assert!(start_line("cube.gcode", &params).is_err());
        }
        params = StartParams { flow_calibrate: Some(false), time_lapse_camera: Some(true), filament_used_mm: vec![1.25,-0.0], layer_height: Some(0.2), slicer: Some("Fab".into()), slicer_version: Some("1".into()), ..Default::default() };
        assert_eq!(start_line("cube.gcode", &params).unwrap(), "SDCARD_PRINT_FILE_WITH_PARAMETERS FILENAME=\"cube.gcode\" FLOW_CALIBRATE=\"0\" TIME_LAPSE_CAMERA=\"1\" LAYER_HEIGHT=\"0.2\" FILAMENT_USED_MM=\"[1.25, 0.0]\" SLICER=\"Fab\" SLICER_VERSION=\"1\"");
        for (value, decimal) in [(0.4, "0.4"), (1.0, "1.0"), (220.0, "220.0"), (0.0, "0.0"), (3.23, "3.23")] {
            assert_eq!(number(value).unwrap(), decimal);
            assert_eq!(decimal.parse::<f64>().unwrap(), value);
        }
        params.layer_height = Some(f64::NAN);
        assert!(start_line("cube.gcode", &params).is_err());
        assert!(start_line("a\".gcode", &StartParams::default()).is_err());
        assert!(start_line("a\\b.gcode", &StartParams::default()).is_err());
        for (code, phrase) in [("531-14", "diameter"), ("531-16", "busy"), ("531-17", "parameter"), ("531-19", "high-flow"), ("531-25", "material")] {
            assert!(refusal(&format!("error: {code} refused")).unwrap().contains(phrase));
        }
        assert_eq!(refusal("531-160"), None);
    }

    #[test]
    fn response_framing_is_bounded_and_chunked() {
        let limits = Limits { header: 128, body: 5 };
        let mut input = Cursor::new(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2;x=y\r\nab\r\n3\r\ncde\r\n0\r\nTrailer: yes\r\n\r\nnext");
        let mut buffered = Input::default();
        let head = buffered.head(&mut input, limits.header, &Waiting::default()).unwrap();
        assert_eq!(buffered.body(&mut input, &head, limits, &Waiting::default()).unwrap(), b"abcde");
        let mut rest = String::from_utf8(buffered.bytes[buffered.position..].to_vec()).unwrap(); input.read_to_string(&mut rest).unwrap(); assert_eq!(rest, "next");
        let mut input = Cursor::new(format!("HTTP/1.1 200 OK\r\nX: {}\r\n\r\n", "x".repeat(128)));
        assert!(matches!(Input::default().head(&mut input, limits.header, &Waiting::default()), Err(Error::Protocol("HTTP headers exceed cap"))));
        for bytes in [
            "HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n6\r\n",
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nContent-Length: 2\r\n\r\n",
        ] {
            let mut input = Cursor::new(bytes);
            let mut buffered = Input::default();
        let head = buffered.head(&mut input, limits.header, &Waiting::default()).unwrap();
            assert!(matches!(buffered.body(&mut input, &head, limits, &Waiting::default()), Err(Error::Protocol(_))));
        }
    }

    #[test]
    fn a_colliding_boundary_is_retried() {
        let mut candidates = ["fab-candidate", "fab-other"].into_iter();
        assert_eq!(boundary_with(b"G1 X1\nfab-candidate\n", || candidates.next().unwrap().into()), "fab-other");
    }

    struct Scripted { input: Cursor<Vec<u8>>, output: Vec<u8> }
    impl Read for Scripted { fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> { self.input.read(bytes) } }
    impl Write for Scripted {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> { self.output.extend_from_slice(bytes); Ok(bytes.len()) }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    fn scripted(response: &[u8]) -> Scripted { Scripted { input: Cursor::new(response.to_vec()), output: Vec::new() } }

    #[test]
    fn names_are_rejected_before_writing_and_headers_are_validated() {
        let mut client = Client::new(scripted(&[]), "memory", None).unwrap();
        for name in ["a/b.gcode", "a\".gcode", "../a.gcode", "a\n.gcode", "a.txt"] {
            assert!(client.upload(name, b"G1", &AtomicBool::new(false), |_,_| {}).is_err());
            assert!(client.stream.as_ref().unwrap().output.is_empty());
        }
        assert!(Client::new(scripted(&[]), "memory\r\nInjected: yes", None).is_err());
        assert!(Client::new(scripted(&[]), "memory", Some(Secret::new(b"bad\nkey".to_vec()))).is_err());
        let invalid = b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: wrong\r\n\r\n";
        assert!(matches!(StatusSocket::connect(scripted(invalid), "memory", None, SIM_HEADS), Err(Error::Protocol("invalid websocket upgrade"))));
    }

    #[test]
    fn authentication_and_errors_never_expose_the_key() {
        let key = "private-test-key";
        let sim = SimMoonraker::new(Some(key.into()));
        let mut client = Client::new(sim.stream(), "memory", None).unwrap();
        assert_eq!(client.server_info(), Err(Error::Unauthorized));
        assert!(matches!(StatusSocket::connect(sim.stream(), "memory", None, SIM_HEADS), Err(Error::Unauthorized)));
        let mut client = Client::new(sim.stream(), "memory", Some(Secret::new(key.as_bytes().to_vec()))).unwrap();
        assert_eq!(client.server_info().unwrap().klippy_state, "ready");
        client.upload("cube.gcode", b"G1", &AtomicBool::new(false), |_,_| {}).unwrap();
        sim.refuse_next_start(&format!("531-16 echoed {key}"));
        let error = client.script(&start_line("cube.gcode", &StartParams::default()).unwrap()).unwrap_err();
        assert!(matches!(error, Error::Refused(_)));
        assert!(!format!("{error} {error:?}").contains(key));
        let mut socket = StatusSocket::connect(sim.stream(), "memory", Some(Secret::new(key.as_bytes().to_vec())), SIM_HEADS).unwrap();
        assert_eq!(socket.poll().unwrap().unwrap().state, "standby");
        for error in [Error::Unauthorized, Error::Http(403), Error::Protocol("invalid API key"), Error::Io, Error::Cancelled] { assert!(!format!("{error} {error:?}").contains(key)); }
    }

    #[test]
    fn deltas_preserve_omitted_fields_and_merge_raw_keys() {
        let mut status = Status::new(4);
        status.merge(&parse(br#"{"print_stats":{"state":"printing","info":{"current_layer":2,"total_layer":9}},"extruder2":{"temperature":170,"target":200},"toolhead":{"extruder":"extruder2"},"exclude_object":{"objects":[{"name":"one"}],"current_object":"one"},"print_task_config":{"a":{"x":1,"y":2}},"filament_detect":{"one":true}}"#).unwrap()).unwrap();
        status.merge(&parse(br#"{"print_stats":{"info":{"current_layer":3}},"extruder2":{"temperature":180},"exclude_object":{"current_object":null},"print_task_config":{"a":{"x":3},"b":true},"filament_detect":{"two":false}}"#).unwrap()).unwrap();
        assert_eq!((status.current_layer, status.total_layer, status.state.as_str()), (3,9,"printing"));
        assert_eq!(status.extruders[2], Heater { temperature: 180.0, target: 200.0 });
        assert_eq!(status.active_extruder, "extruder2");
        assert_eq!(status.objects, ["one"]); assert_eq!(status.current_object, None);
        assert_eq!(status.print_task_config.get("a").unwrap().get("y"), Some(&Value::Int(2)));
        assert_eq!(status.filament_detect.get("one"), Some(&Value::Bool(true)));
    }
    struct Pausing<S> { inner: S, reads: usize, writes: usize, kind: io::ErrorKind }
    impl<S: Read> Read for Pausing<S> {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads % 2 == 1 { return Err(self.kind.into()); }
            let count = bytes.len().min(1);
            self.inner.read(&mut bytes[..count])
        }
    }
    impl<S: Write> Write for Pausing<S> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            if self.writes % 2 == 1 { return Err(self.kind.into()); }
            self.inner.write(&bytes[..bytes.len().min(3)])
        }
        fn flush(&mut self) -> io::Result<()> { self.inner.flush() }
    }
    fn http_answer(body: &str) -> String {
        format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len())
    }
    #[test]
    fn http_pauses_preserve_every_byte_and_the_next_response() {
        let body = r#"{"result":{"status":{"value":"kept"}}}"#;
        let fixed = http_answer(body);
        let chunked = format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n", 5, &body[..5], body.len()-5, &body[5..]);
        for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::TimedOut, io::ErrorKind::Interrupted] {
        for response in [&fixed, &chunked] {
            let replies = format!("{response}{}", http_answer(body));
            let mut client = Client::new(Pausing { inner: scripted(replies.as_bytes()), reads: 0, writes: 0, kind }, "memory", None).unwrap();
            for _ in 0..2 {
                assert_eq!(client.query(&["value"]).unwrap(), obj(vec![("value", s("kept"))]));
            }
            let expected = "GET /printer/objects/query?value HTTP/1.1\r\nHost: memory\r\nConnection: keep-alive\r\nContent-Length: 0\r\nContent-Type: application/json\r\n\r\n".repeat(2);
            assert_eq!(client.stream.as_ref().unwrap().inner.output, expected.as_bytes());
        }
        }
        // A read may include the next response as well.
        let mut client = Client::new(scripted(format!("{}{}", http_answer(body), http_answer(body)).as_bytes()), "memory", None).unwrap();
        assert_eq!(client.query(&["value"]).unwrap(), client.query(&["value"]).unwrap());
        let sim = SimMoonraker::default();
        let mut socket = StatusSocket::connect(Pausing { inner: sim.stream(), reads: 0, writes: 0, kind: io::ErrorKind::WouldBlock }, "memory", None, SIM_HEADS).unwrap();
        while socket.poll().unwrap().is_none() {}
        assert_eq!(socket.status().state, "standby", "upgrade and identification survive partial writes");
    }

    struct Failing {
        written: std::rc::Rc<std::cell::RefCell<Vec<u8>>>,
        signal: Option<Arc<AtomicBool>>,
        during_write: bool,
    }
    impl Failing {
        fn fail(&self) -> io::Error {
            if let Some(signal) = &self.signal {
                signal.store(true, Ordering::Release);
                io::ErrorKind::WouldBlock.into()
            } else { io::ErrorKind::BrokenPipe.into() }
        }
    }
    impl Read for Failing {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(self.fail()) }
    }
    impl Write for Failing {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.during_write && !self.written.borrow().is_empty() { return Err(self.fail()); }
            let count = if self.during_write { bytes.len().min(3) } else { bytes.len() };
            self.written.borrow_mut().extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    #[test]
    fn stopped_and_failed_exchanges_drop_the_stream() {
        for during_write in [false, true] {
            for stopped in [false, true] {
                let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
                let stop = Arc::new(AtomicBool::new(false));
                let stream = Failing { written: written.clone(), signal: stopped.then(|| stop.clone()), during_write };
                let mut client = Client::new(stream, "memory", None).unwrap();
                client.set_stop(stop);
                assert_eq!(client.server_info(), Err(if stopped { Error::Cancelled } else { Error::Io }));
                let before = written.borrow().clone();
                assert_eq!(client.server_info(), Err(Error::Protocol("stream is closed")));
                assert_eq!(*written.borrow(), before, "a failed exchange must not append another request");
            }
        }
        // Upload's own flag must also interrupt waits before and after its body.
        for during_write in [false, true] {
            let cancel = Arc::new(AtomicBool::new(false));
            let stream = Failing { written: Default::default(), signal: Some(cancel.clone()), during_write };
            let mut client = Client::new(stream, "memory", None).unwrap();
            assert_eq!(client.upload("cube.gcode", b"G1 X1", &cancel, |_,_| {}), Err(Error::Cancelled));
            assert_eq!(client.server_info(), Err(Error::Protocol("stream is closed")));
        }
    }
    #[test]
    fn advertised_extreme_lengths_do_not_allocate_or_panic() {
        for response in [
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\nx", usize::MAX),
            "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nffffffffffffffff\r\nx".into(),
        ] {
            let mut client = Client::new(scripted(response.as_bytes()), "memory", None).unwrap();
            client.set_limits(Limits { header: Limits::default().header, body: usize::MAX });
            assert!(client.query(&["value"]).is_err());
            assert!(client.stream.is_none());
        }
    }
    #[test]
    fn response_and_rpc_redaction_cover_keys_arrays_and_paths() {
        let key = "private-test-key";
        let value = obj(vec![("result", obj(vec![("item", obj(vec![("path", s(format!("{key}.gcode")))]))]))]);
        let mut client = Client::new(scripted(http_answer(&value.to_json()).as_bytes()), "memory", Some(Secret::new(key.as_bytes().to_vec()))).unwrap();
        assert_eq!(client.upload("cube.gcode", b"", &AtomicBool::new(false), |_,_| {}).unwrap(), "***.gcode");
        let value = obj(vec![("result", obj(vec![("klippy_state", s("ready")), ("moonraker_version", s(key))]))]);
        let mut client = Client::new(scripted(http_answer(&value.to_json()).as_bytes()), "memory", Some(Secret::new(key.as_bytes().to_vec()))).unwrap();
        assert_eq!(client.server_info().unwrap().version, "***");
        let sim = SimMoonraker::new(Some(key.into()));
        let mut socket = StatusSocket::connect(sim.stream(), "memory", Some(Secret::new(key.as_bytes().to_vec())), SIM_HEADS).unwrap();
        let error = obj(vec![("jsonrpc", s("2.0")), ("id", Value::Int(1)), ("error", obj(vec![("message", s(key))]))]);
        let error = socket.message(error.to_json().as_bytes()).unwrap_err();
        assert_eq!(error, Error::Refused("***".into()));
        assert!(!format!("{error} {error:?}").contains(key));
    }
}

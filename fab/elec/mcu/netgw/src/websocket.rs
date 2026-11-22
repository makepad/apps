use std::collections::VecDeque;
use crate::{base64, sha1, Message, Stats};

const BAD_REQUEST: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

pub fn accept(key: &str) -> String {
    let mut bytes = key.as_bytes().to_vec();
    bytes.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
    base64::encode(&sha1::digest(&bytes))
}

pub(crate) fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x80 | opcode];
    match payload.len() {
        0..=125 => out.push(payload.len() as u8),
        126..=65535 => { out.push(126); out.extend_from_slice(&(payload.len() as u16).to_be_bytes()); }
        _ => { out.push(127); out.extend_from_slice(&(payload.len() as u64).to_be_bytes()); }
    }
    out.extend_from_slice(payload);
    out
}

pub(crate) fn close_frame(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    payload.extend_from_slice(reason.as_bytes());
    frame(8, &payload)
}

pub(crate) fn valid_code(code: u16) -> bool {
    matches!(code, 1000..=1003 | 1007..=1014 | 3000..=4999)
}

pub(crate) enum Event {
    Open(String),
    Message(Message),
    Closed(u16, String),
    Send(Vec<u8>),
    Finish,
}

#[derive(Clone, Default)]
pub(crate) struct WebSocket {
    input: VecDeque<u8>,
    upgraded: bool,
    closing: bool,
    stopped: bool,
    fragment: Option<(u8, Vec<u8>)>,
}

impl WebSocket {
    pub fn is_open(&self) -> bool { self.upgraded && !self.closing && !self.stopped }

    pub fn host_close(&mut self) { self.closing = true; }

    pub fn eof(&mut self, stats: &mut Stats) -> Option<Vec<u8>> {
        if self.upgraded || self.stopped { return None; }
        stats.http_bad_request += 1;
        self.stopped = true;
        self.input.clear();
        Some(BAD_REQUEST.to_vec())
    }

    fn fail(&mut self, code: u16, stats: &mut Stats, events: &mut Vec<Event>) {
        if code == 1007 { stats.ws_utf8 += 1; } else { stats.ws_protocol += 1; }
        if !self.closing { events.push(Event::Send(close_frame(code, ""))); }
        events.push(Event::Closed(code, String::new()));
        events.push(Event::Finish);
        self.stopped = true;
        self.input.clear();
        self.fragment = None;
    }

    pub fn receive(&mut self, bytes: &[u8], stats: &mut Stats) -> Vec<Event> {
        let mut events = Vec::new();
        if self.stopped {
            stats.ws_after_close_bytes += bytes.len() as u64;
            return events;
        }
        self.input.extend(bytes);
        if !self.upgraded {
            let input = self.input.make_contiguous();
            let Some(end) = input.windows(4).position(|v| v == b"\r\n\r\n").map(|n| n + 4) else { return events; };
            let request = upgrade(&input[..end]);
            self.input.drain(..end);
            if let Some((path, key)) = request {
                let response = format!("HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\n\r\n", accept(&key));
                events.push(Event::Send(response.into_bytes()));
                events.push(Event::Open(path));
                self.upgraded = true;
                stats.ws_opened += 1;
            } else {
                stats.http_bad_request += 1;
                events.push(Event::Send(BAD_REQUEST.to_vec()));
                events.push(Event::Finish);
                self.stopped = true;
                self.input.clear();
                return events;
            }
        }
        while self.input.len() >= 2 {
            let a = self.input[0];
            let b = self.input[1];
            let opcode = a & 15;
            let fin = a & 128 != 0;
            let control = opcode & 8 != 0;
            if b & 128 == 0 {
                stats.ws_unmasked += 1;
                self.fail(1002, stats, &mut events);
                break;
            }
            if a & 0x70 != 0 || !matches!(opcode, 0..=2 | 8..=10)
                || (control && (!fin || b & 127 > 125))
                || (!control && ((opcode == 0) != self.fragment.is_some())) {
                self.fail(1002, stats, &mut events);
                break;
            }
            let short = b & 127;
            let extra = match short { 126 => 2, 127 => 8, _ => 0 };
            let header = 2 + extra + 4;
            if self.input.len() < header { break; }
            let mut len = u64::from(short);
            if extra != 0 {
                len = 0;
                for i in 2..2 + extra { len = (len << 8) | u64::from(self.input[i]); }
                if (extra == 2 && len < 126) || (extra == 8 && (len < 65536 || len >> 63 != 0)) {
                    self.fail(1002, stats, &mut events);
                    break;
                }
            }
            // Compare in u64 before converting, also on a 32-bit wasm host.
            if len > (self.input.len() - header) as u64 { break; }
            let mask = [self.input[header - 4], self.input[header - 3], self.input[header - 2], self.input[header - 1]];
            self.input.drain(..header);
            let payload: Vec<u8> = self.input.drain(..len as usize).enumerate().map(|(i, b)| b ^ mask[i % 4]).collect();
            match opcode {
                8 => {
                    if payload.len() == 1 {
                        self.fail(1002, stats, &mut events);
                        break;
                    }
                    let (code, reason) = if payload.is_empty() { (1005, String::new()) } else {
                        let code = u16::from_be_bytes([payload[0], payload[1]]);
                        if !valid_code(code) { self.fail(1002, stats, &mut events); break; }
                        let Ok(reason) = std::str::from_utf8(&payload[2..]) else {
                            self.fail(1007, stats, &mut events); break;
                        };
                        (code, reason.to_owned())
                    };
                    if !self.closing { events.push(Event::Send(frame(8, &payload))); }
                    events.push(Event::Closed(code, reason));
                    events.push(Event::Finish);
                    self.stopped = true;
                    stats.ws_after_close_bytes += self.input.len() as u64;
                    self.input.clear();
                    self.fragment = None;
                    break;
                }
                9 => events.push(Event::Send(frame(10, &payload))),
                10 => {},
                _ if self.closing => { stats.ws_after_close_bytes += payload.len() as u64; }
                _ => {
                    let (kind, data) = if opcode == 0 {
                        let (kind, mut data) = self.fragment.take().unwrap();
                        data.extend_from_slice(&payload);
                        (kind, data)
                    } else { (opcode, payload) };
                    if !fin { self.fragment = Some((kind, data)); continue; }
                    let msg = if kind == 1 {
                        let Ok(text) = String::from_utf8(data) else { self.fail(1007, stats, &mut events); break; };
                        Message::Text(text)
                    } else { Message::Binary(data) };
                    stats.ws_messages_in += 1;
                    stats.ws_bytes_in += msg.len() as u64;
                    events.push(Event::Message(msg));
                }
            }
        }
        events
    }
}

fn upgrade(bytes: &[u8]) -> Option<(String, String)> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next()?.split(' ');
    if request.next()? != "GET" { return None; }
    let path = request.next()?;
    if !path.starts_with('/') || path.bytes().any(|b| b <= 32 || b == 127)
        || request.next()? != "HTTP/1.1" || request.next().is_some() { return None; }
    let mut host = false;
    let mut upgraded = false;
    let mut connection = false;
    let mut version = None;
    let mut key = None;
    for line in lines {
        if line.is_empty() { break; }
        let (name, value) = line.split_once(':')?;
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)) { return None; }
        let value = value.trim();
        if value.bytes().any(|b| b < 32 && b != b'\t' || b == 127) { return None; }
        if name.eq_ignore_ascii_case("host") { host = !value.is_empty(); }
        if name.eq_ignore_ascii_case("upgrade") { upgraded |= value.eq_ignore_ascii_case("websocket"); }
        if name.eq_ignore_ascii_case("connection") { connection |= value.split(',').any(|v| v.trim().eq_ignore_ascii_case("upgrade")); }
        if name.eq_ignore_ascii_case("sec-websocket-version") {
            if version.replace(value).is_some() { return None; }
        }
        if name.eq_ignore_ascii_case("sec-websocket-key") {
            if key.replace(value).is_some() { return None; }
        }
    }
    let key = key?;
    if !host || !upgraded || !connection || version? != "13" || base64::decode(key)?.len() != 16 { return None; }
    Some((path.to_owned(), key.to_owned()))
}

//! The printer's live camera: RTSP over TLS (port 322), digest login,
//! RTP interleaved on the one TLS connection (no UDP), H.264 depacketized
//! (single NAL units, STAP-A, FU-A) into Annex-B access units. Decoding is
//! the caller's (the platform stream decoder).
//!
//! The printer serves one live viewer at a time: the stream is opened only
//! while a camera view is shown, and torn down when it closes.

use crate::secrets::Secret;
use makepad_network::digest::md5_hash;
use std::io;

pub const CAMERA_PATH: &str = "/streaming/live/1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CameraError {
    Connect(&'static str),
    /// The printer said no to a request (401 after login: wrong code; 453/503: another viewer).
    Status { request: &'static str, code: u16 },
    Protocol(&'static str),
    Transport,
}

impl CameraError {
    pub fn message(&self) -> String {
        match self {
            Self::Connect(why) => (*why).to_string(),
            Self::Status { code: 401, .. } => "the printer refused the camera login (access code)".into(),
            Self::Status { code: 453 | 503, .. } => "another viewer is watching the printer camera (one at a time)".into(),
            Self::Status { request, code } => format!("camera {request}: printer answered {code}"),
            Self::Protocol(why) => format!("camera: {why}"),
            Self::Transport => "camera connection lost".into(),
        }
    }
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }

/// RFC 2617 digest without qop: MD5(MD5(user:realm:pass):nonce:MD5(method:uri)).
pub fn digest_response(user: &str, realm: &str, password: &Secret, nonce: &str, method: &str, uri: &str) -> String {
    let mut a1 = Secret::new(format!("{user}:{realm}:").into_bytes());
    a1.extend(password.bytes());
    let ha1 = Secret::new(hex(&md5_hash(a1.bytes())).into_bytes());
    let ha2 = hex(&md5_hash(format!("{method}:{uri}").as_bytes()));
    let mut response = Secret::new(Vec::new());
    response.extend(ha1.bytes());
    response.extend(format!(":{nonce}:{ha2}").as_bytes());
    hex(&md5_hash(response.bytes()))
}

/// `Digest realm="x", nonce="y"` -> (realm, nonce).
pub fn parse_challenge(header: &str) -> Option<(String, String)> {
    let rest = header.trim().strip_prefix("Digest")?.trim();
    let mut realm = None;
    let mut nonce = None;
    for part in split_params(rest) {
        let (k, v) = part.split_once('=')?;
        let v = v.trim().trim_matches('"').to_string();
        match k.trim().to_ascii_lowercase().as_str() { "realm" => realm = Some(v), "nonce" => nonce = Some(v), _ => {} }
    }
    Some((realm?, nonce?))
}

fn split_params(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted) = (0, false);
    for (i, c) in s.char_indices() {
        match c { '"' => quoted = !quoted, ',' if !quoted => { out.push(s[start..i].trim()); start = i + 1; } _ => {} }
    }
    if start < s.len() { out.push(s[start..].trim()); }
    out
}

/// How often a playing session must be kept alive: the H2D ends a session after about 15 s without a request
/// (measured: `live_camera_session`; its SETUP reply names no timeout), and stays up with one every 5 s.
pub const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Response { pub code: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// What the SDP says about the video track.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track { pub control: String, pub payload_type: u8, pub parameter_sets: Vec<Vec<u8>> }

fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for c in text.bytes() {
        let v = match c { b'A'..=b'Z' => c - b'A', b'a'..=b'z' => c - b'a' + 26, b'0'..=b'9' => c - b'0' + 52, b'+' => 62, b'/' => 63, b'=' => break, _ => return None };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 { bits -= 8; out.push((acc >> bits) as u8); }
    }
    Some(out)
}

pub fn parse_sdp(sdp: &str) -> Option<Track> {
    let mut track: Option<Track> = None;
    for line in sdp.lines().map(str::trim) {
        if let Some(m) = line.strip_prefix("m=") {
            if track.is_some() { break; } // the first video track only
            if !m.starts_with("video") { continue; }
            let pt = m.split_whitespace().nth(3)?.parse().ok()?;
            track = Some(Track { payload_type: pt, ..Default::default() });
            continue;
        }
        let Some(t) = track.as_mut() else { continue };
        if let Some(c) = line.strip_prefix("a=control:") { t.control = c.trim().to_string(); }
        if let Some(f) = line.strip_prefix("a=fmtp:") {
            for p in f.split(';') {
                if let Some(v) = p.trim().strip_prefix("sprop-parameter-sets=") {
                    t.parameter_sets = v.split(',').filter_map(base64_decode).filter(|n| !n.is_empty()).collect();
                }
            }
        }
    }
    track.filter(|t| !t.control.is_empty())
}

/// The control URL of a track: absolute, or relative to the content base.
pub fn control_url(base: &str, control: &str) -> String {
    if control.starts_with("rtsp://") || control.starts_with("rtsps://") { return control.to_string(); }
    if control == "*" { return base.to_string(); }
    format!("{}/{}", base.trim_end_matches('/'), control.trim_start_matches('/'))
}

/// RTP payloads in, Annex-B access units out (an access unit ends at the marker bit
/// or a timestamp change). Units with a lost fragment are dropped up to the next one.
#[derive(Default)]
pub struct Depacketizer {
    unit: Vec<u8>,
    timestamp: Option<u32>,
    fragment: Option<Vec<u8>>,
    broken: bool,
    last_seq: Option<u16>,
    parameter_sets: Vec<Vec<u8>>,
    has_parameter_sets: bool,
    keyframe: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccessUnit { pub annex_b: Vec<u8>, pub timestamp: u32, pub keyframe: bool }

impl Depacketizer {
    /// `parameter_sets` (SPS, PPS from the SDP) precede a keyframe that does not carry its own.
    pub fn new(parameter_sets: Vec<Vec<u8>>) -> Self { Self { parameter_sets, ..Default::default() } }

    fn nal(&mut self, nal: &[u8]) {
        let Some(&head) = nal.first() else { return };
        match head & 0x1f {
            5 => self.keyframe = true,
            7 | 8 => self.has_parameter_sets = true,
            _ => {}
        }
        self.unit.extend_from_slice(&[0, 0, 0, 1]);
        self.unit.extend_from_slice(nal);
    }

    fn finish(&mut self) -> Option<AccessUnit> {
        let timestamp = self.timestamp.take()?;
        let mut annex_b = std::mem::take(&mut self.unit);
        let keyframe = std::mem::take(&mut self.keyframe);
        let has_sets = std::mem::take(&mut self.has_parameter_sets);
        self.fragment = None;
        if std::mem::take(&mut self.broken) || annex_b.is_empty() { return None; }
        if keyframe && !has_sets && !self.parameter_sets.is_empty() {
            let mut with = Vec::new();
            for set in &self.parameter_sets { with.extend_from_slice(&[0, 0, 0, 1]); with.extend_from_slice(set); }
            with.append(&mut annex_b);
            annex_b = with;
        }
        Some(AccessUnit { annex_b, timestamp, keyframe })
    }

    /// One RTP packet (header included). Returns the access units it completes.
    pub fn push(&mut self, packet: &[u8]) -> Vec<AccessUnit> {
        let mut out = Vec::new();
        if packet.len() < 12 || packet[0] >> 6 != 2 { return out; }
        let csrc = (packet[0] & 0x0f) as usize;
        let marker = packet[1] & 0x80 != 0;
        let seq = u16::from_be_bytes([packet[2], packet[3]]);
        let timestamp = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        let mut start = 12 + 4 * csrc;
        if packet[0] & 0x10 != 0 {
            let Some(ext) = packet.get(start + 2..start + 4) else { return out };
            start += 4 + 4 * u16::from_be_bytes([ext[0], ext[1]]) as usize;
        }
        let mut end = packet.len();
        if packet[0] & 0x20 != 0 { end = end.saturating_sub(*packet.last().unwrap() as usize); }
        let Some(payload) = packet.get(start..end).filter(|p| !p.is_empty()) else { return out };
        if self.last_seq.is_some_and(|last| seq != last.wrapping_add(1)) { self.broken = true; self.fragment = None; }
        self.last_seq = Some(seq);
        if self.timestamp.is_some_and(|t| t != timestamp) { out.extend(self.finish()); }
        self.timestamp = Some(timestamp);
        match payload[0] & 0x1f {
            1..=23 => self.nal(payload),
            24 => {
                // STAP-A: 16-bit sizes, each followed by a NAL unit.
                let mut rest = &payload[1..];
                while rest.len() >= 2 {
                    let n = u16::from_be_bytes([rest[0], rest[1]]) as usize;
                    let Some(nal) = rest.get(2..2 + n) else { self.broken = true; break };
                    self.nal(nal);
                    rest = &rest[2 + n..];
                }
            }
            28 if payload.len() >= 2 => {
                let (indicator, header) = (payload[0], payload[1]);
                if header & 0x80 != 0 {
                    let mut f = vec![(indicator & 0xe0) | (header & 0x1f)];
                    f.extend_from_slice(&payload[2..]);
                    self.fragment = Some(f);
                } else if let Some(f) = self.fragment.as_mut() {
                    f.extend_from_slice(&payload[2..]);
                } else {
                    self.broken = true;
                }
                if header & 0x40 != 0 {
                    if let Some(f) = self.fragment.take() { self.nal(&f); }
                }
            }
            _ => {}
        }
        if marker { out.extend(self.finish()); }
        out
    }
}

/// What the RTSP connection delivers after PLAY.
pub enum Incoming { Rtp { channel: u8, packet: Vec<u8> }, Response(Response) }

pub struct Rtsp<S: io::Read + io::Write> {
    stream: S,
    buffer: Vec<u8>,
    cseq: u32,
    url: String,
    auth: Option<(String, String)>,
    session: Option<String>,
    password: Secret,
}

impl<S: io::Read + io::Write> Rtsp<S> {
    pub fn new(stream: S, url: String, password: &Secret) -> Self {
        Self { stream, buffer: Vec::new(), cseq: 0, url, auth: None, session: None, password: Secret::new(password.bytes().to_vec()) }
    }

    fn fill(&mut self) -> Result<(), CameraError> {
        let mut chunk = [0u8; 16384];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err(CameraError::Transport),
                Ok(n) => { self.buffer.extend_from_slice(&chunk[..n]); return Ok(()); }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => return Err(CameraError::Transport),
                Err(_) => return Err(CameraError::Transport),
            }
        }
    }

    /// The next interleaved packet or response.
    pub fn next(&mut self) -> Result<Incoming, CameraError> {
        loop {
            if let Some(item) = self.parse()? { return Ok(item); }
            if self.buffer.len() > 4 * 1024 * 1024 { return Err(CameraError::Protocol("oversized message")); }
            self.fill()?;
        }
    }

    fn parse(&mut self) -> Result<Option<Incoming>, CameraError> {
        if self.buffer.first() == Some(&b'$') {
            if self.buffer.len() < 4 { return Ok(None); }
            let n = u16::from_be_bytes([self.buffer[2], self.buffer[3]]) as usize;
            if self.buffer.len() < 4 + n { return Ok(None); }
            let channel = self.buffer[1];
            let packet = self.buffer[4..4 + n].to_vec();
            self.buffer.drain(..4 + n);
            return Ok(Some(Incoming::Rtp { channel, packet }));
        }
        if self.buffer.is_empty() { return Ok(None); }
        if !self.buffer.starts_with(b"RTSP/".get(..self.buffer.len().min(5)).unwrap()) { return Err(CameraError::Protocol("unexpected data")); }
        let Some(head_end) = self.buffer.windows(4).position(|w| w == b"\r\n\r\n") else { return Ok(None) };
        let head = String::from_utf8_lossy(&self.buffer[..head_end]).into_owned();
        let mut lines = head.split("\r\n");
        let status = lines.next().unwrap_or("");
        let code = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or(CameraError::Protocol("malformed status line"))?;
        let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_string(), v.trim().to_string())).collect();
        let length: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
        let total = head_end + 4 + length;
        if self.buffer.len() < total { return Ok(None); }
        let body = self.buffer[head_end + 4..total].to_vec();
        self.buffer.drain(..total);
        Ok(Some(Incoming::Response(Response { code, headers, body })))
    }

    pub(crate) fn send(&mut self, method: &'static str, uri: &str, extra: &[(&str, String)]) -> Result<u32, CameraError> {
        self.cseq += 1;
        let mut request = format!("{method} {uri} RTSP/1.0\r\nCSeq: {}\r\nUser-Agent: Fab\r\n", self.cseq);
        if let Some((realm, nonce)) = &self.auth {
            let response = digest_response("bblp", realm, &self.password, nonce, method, uri);
            request.push_str(&format!("Authorization: Digest username=\"bblp\", realm=\"{realm}\", nonce=\"{nonce}\", uri=\"{uri}\", response=\"{response}\"\r\n"));
        }
        if let Some(session) = &self.session { request.push_str(&format!("Session: {session}\r\n")); }
        for (k, v) in extra { request.push_str(&format!("{k}: {v}\r\n")); }
        request.push_str("\r\n");
        if request.bytes().filter(|b| *b == b'\n').count() != request.matches("\r\n").count() { return Err(CameraError::Protocol("header contains a line break")); }
        self.stream.write_all(request.as_bytes()).map_err(|_| CameraError::Transport)?;
        self.stream.flush().map_err(|_| CameraError::Transport)?;
        Ok(self.cseq)
    }

    /// A request and its response (interleaved data before it is dropped: only before PLAY).
    fn request(&mut self, method: &'static str, uri: &str, extra: &[(&str, String)]) -> Result<Response, CameraError> {
        for attempt in 0..2 {
            self.send(method, uri, extra)?;
            let response = loop {
                if let Incoming::Response(r) = self.next()? { break r; }
            };
            if response.code == 401 && attempt == 0 && self.auth.is_none() {
                let challenge = response.header("WWW-Authenticate").and_then(parse_challenge).ok_or(CameraError::Protocol("401 without a digest challenge"))?;
                self.auth = Some(challenge);
                continue;
            }
            if response.code != 200 { return Err(CameraError::Status { request: method, code: response.code }); }
            return Ok(response);
        }
        Err(CameraError::Status { request: method, code: 401 })
    }

    /// OPTIONS, DESCRIBE (digest), SETUP (TCP interleaved 0-1), PLAY. Returns the video track.
    pub fn start(&mut self) -> Result<Track, CameraError> {
        let url = self.url.clone();
        self.request("OPTIONS", &url, &[])?;
        let describe = self.request("DESCRIBE", &url, &[("Accept", "application/sdp".into())])?;
        let track = parse_sdp(&String::from_utf8_lossy(&describe.body)).ok_or(CameraError::Protocol("no H.264 video track"))?;
        let base = describe.header("Content-Base").unwrap_or(&url).to_string();
        let setup_url = control_url(&base, &track.control);
        let setup = self.request("SETUP", &setup_url, &[("Transport", "RTP/AVP/TCP;unicast;interleaved=0-1".into())])?;
        let session = setup.header("Session").ok_or(CameraError::Protocol("SETUP without a session"))?;
        self.session = Some(session.split(';').next().unwrap_or(session).trim().to_string());
        self.send("PLAY", &base, &[("Range", "npt=0.000-".into())])?;
        Ok(track)
    }

    /// Keeps the session alive (the reply arrives among the packets). Send it every [`KEEPALIVE`].
    pub fn url(&self) -> String { self.url.clone() }

    pub fn keepalive(&mut self) -> Result<(), CameraError> {
        let url = self.url.clone();
        self.send("GET_PARAMETER", &url, &[]).map(|_| ())
    }

    pub fn teardown(mut self) -> S {
        let url = self.url.clone();
        let _ = self.send("TEARDOWN", &url, &[]);
        self.stream
    }
}

/// Opens the printer's camera over TLS, checking the leaf against the printer's pin.
#[cfg(not(target_arch = "wasm32"))]
pub fn open(conf_name: &str) -> Result<Rtsp<makepad_network::SocketStream>, CameraError> {
    let config = crate::secrets::PrinterConfig::load(conf_name).map_err(CameraError::Connect)?;
    let crate::secrets::Access::Code(code) = &config.access else { return Err(CameraError::Connect("this printer's camera is not supported")) };
    let pin = crate::tls::pinned_leaf(conf_name).ok_or(CameraError::Connect("the printer's certificate is not pinned yet (connect first)"))?;
    let (mut stream, leaf) = makepad_network::SocketStream::connect_capture(&config.host, "322").map_err(|_| CameraError::Connect("printer camera connection failed"))?;
    if makepad_network::tls::fingerprint(&leaf) != pin {
        stream.shutdown();
        return Err(CameraError::Connect("printer camera certificate is not the pinned printer certificate"));
    }
    stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).map_err(|_| CameraError::Transport)?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(5))).map_err(|_| CameraError::Transport)?;
    Ok(Rtsp::new(stream, format!("rtsps://{}:322{CAMERA_PATH}", config.host), code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::{Read, Write};

    #[test]
    fn digest_matches_the_rfc_2617_example() {
        // RFC 2617's example inputs without qop (the value cross-checked with another MD5 implementation).
        let r = digest_response("Mufasa", "testrealm@host.com", &Secret::new(b"Circle Of Life".to_vec()), "dcd98b7102dd2f0e8b11d0f600bfb0c093", "GET", "/dir/index.html");
        assert_eq!(r, "670fd8c2df070c60b045671b8b24ff02");
        assert_eq!(parse_challenge(r#"Digest realm="LIVE555 Streaming Media", nonce="abc,def""#), Some(("LIVE555 Streaming Media".into(), "abc,def".into())));
        assert_eq!(parse_challenge("Basic realm=x"), None);
    }

    const SDP: &str = "v=0\r\no=- 1 1 IN IP4 0.0.0.0\r\ns=Session\r\nt=0 0\r\na=control:*\r\na=range:npt=now-\r\nm=video 0 RTP/AVP 96\r\nc=IN IP4 0.0.0.0\r\nb=AS:1000\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=64C029;sprop-parameter-sets=Z2QAKaw=,aO48gA==\r\na=control:track1\r\n";

    #[test]
    fn sdp_track_control_and_parameter_sets() {
        let t = parse_sdp(SDP).unwrap();
        assert_eq!(t.control, "track1");
        assert_eq!(t.payload_type, 96);
        assert_eq!(t.parameter_sets, vec![vec![0x67, 0x64, 0x00, 0x29, 0xac], vec![0x68, 0xee, 0x3c, 0x80]]);
        assert_eq!(control_url("rtsps://p:322/streaming/live/1/", "track1"), "rtsps://p:322/streaming/live/1/track1");
        assert_eq!(control_url("rtsps://p:322/a", "rtsps://p:322/a/track1"), "rtsps://p:322/a/track1");
    }

    fn rtp(seq: u16, ts: u32, marker: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x80, 96 | if marker { 0x80 } else { 0 }];
        p.extend_from_slice(&seq.to_be_bytes());
        p.extend_from_slice(&ts.to_be_bytes());
        p.extend_from_slice(&[0, 0, 0, 1]);
        p.extend_from_slice(payload);
        p
    }

    #[test]
    fn depacketizer_single_stap_fua_and_loss() {
        let mut d = Depacketizer::new(vec![vec![0x67, 1], vec![0x68, 2]]);
        // An IDR split in three FU-A fragments: parameter sets are prepended.
        assert!(d.push(&rtp(1, 100, false, &[0x7c, 0x85, 0xaa])).is_empty());
        assert!(d.push(&rtp(2, 100, false, &[0x7c, 0x05, 0xbb])).is_empty());
        let units = d.push(&rtp(3, 100, true, &[0x7c, 0x45, 0xcc]));
        assert_eq!(units, vec![AccessUnit { annex_b: vec![0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 0xaa, 0xbb, 0xcc], timestamp: 100, keyframe: true }]);
        // STAP-A carrying its own SPS/PPS + a single NAL slice; ends by timestamp change.
        assert!(d.push(&rtp(4, 200, false, &[0x18, 0, 2, 0x67, 9, 0, 2, 0x68, 8])).is_empty());
        assert!(d.push(&rtp(5, 200, false, &[0x65, 0x11])).is_empty());
        let units = d.push(&rtp(6, 300, true, &[0x41, 0x22]));
        assert_eq!(units.len(), 2);
        assert_eq!(units[0].annex_b, vec![0, 0, 0, 1, 0x67, 9, 0, 0, 0, 1, 0x68, 8, 0, 0, 0, 1, 0x65, 0x11], "own parameter sets: none prepended");
        assert_eq!(units[1], AccessUnit { annex_b: vec![0, 0, 0, 1, 0x41, 0x22], timestamp: 300, keyframe: false });
        // A lost packet drops that unit only.
        d.push(&rtp(7, 400, false, &[0x7c, 0x81, 1]));
        assert!(d.push(&rtp(9, 400, true, &[0x7c, 0x41, 3])).is_empty());
        assert_eq!(d.push(&rtp(10, 500, true, &[0x41, 5])).len(), 1);
        assert!(d.push(&[0x80, 0x60]).is_empty(), "short packets are ignored");
    }

    struct Printer { incoming: VecDeque<u8>, sent: Vec<String>, pending: Vec<u8>, code_ok: bool }
    impl Read for Printer {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let n = out.len().min(self.incoming.len()).min(13);
            if n == 0 { return Err(io::ErrorKind::TimedOut.into()); }
            for b in out.iter_mut().take(n) { *b = self.incoming.pop_front().unwrap(); }
            Ok(n)
        }
    }
    impl Write for Printer {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.pending.extend_from_slice(b);
            while let Some(end) = self.pending.windows(4).position(|w| w == b"\r\n\r\n") {
                let request = String::from_utf8(self.pending.drain(..end + 4).collect()).unwrap();
                let method = request.split(' ').next().unwrap().to_string();
                let cseq = request.lines().find_map(|l| l.strip_prefix("CSeq: ")).unwrap().to_string();
                let authed = request.contains("Authorization: Digest");
                self.sent.push(format!("{method}{}", if authed { "+auth" } else { "" }));
                let reply = |code: &str, extra: &str, body: &str| format!("RTSP/1.0 {code}\r\nCSeq: {cseq}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len());
                let text = match method.as_str() {
                    "OPTIONS" => reply("200 OK", "Public: OPTIONS, DESCRIBE, SETUP, TEARDOWN, PLAY\r\n", ""),
                    "DESCRIBE" if !authed => reply("401 Unauthorized", "WWW-Authenticate: Digest realm=\"LIVE555 Streaming Media\", nonce=\"n1\"\r\n", ""),
                    "DESCRIBE" if !self.code_ok => reply("401 Unauthorized", "WWW-Authenticate: Digest realm=\"LIVE555 Streaming Media\", nonce=\"n2\"\r\n", ""),
                    "DESCRIBE" => {
                        let expected = digest_response("bblp", "LIVE555 Streaming Media", &Secret::new(b"TESTCODE".to_vec()), "n1", "DESCRIBE", "rtsps://printer:322/streaming/live/1");
                        assert!(request.contains(&format!("response=\"{expected}\"")));
                        reply("200 OK", "Content-Base: rtsps://printer:322/streaming/live/1/\r\nContent-Type: application/sdp\r\n", SDP)
                    }
                    "SETUP" => {
                        assert!(request.starts_with("SETUP rtsps://printer:322/streaming/live/1/track1 "));
                        assert!(request.contains("Transport: RTP/AVP/TCP;unicast;interleaved=0-1"));
                        reply("200 OK", "Transport: RTP/AVP/TCP;unicast;interleaved=0-1\r\nSession: 5EC7;timeout=65\r\n", "")
                    }
                    "PLAY" => {
                        assert!(request.contains("Session: 5EC7\r\n"));
                        let mut r = reply("200 OK", "Session: 5EC7\r\n", "").into_bytes();
                        let packet = rtp(1, 9000, true, &[0x41, 0x99]);
                        r.extend_from_slice(&[b'$', 0]);
                        r.extend_from_slice(&(packet.len() as u16).to_be_bytes());
                        r.extend_from_slice(&packet);
                        self.incoming.extend(r);
                        continue;
                    }
                    _ => reply("200 OK", "", ""),
                };
                self.incoming.extend(text.into_bytes());
            }
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    #[test]
    fn rtsp_session_logs_in_sets_up_interleaved_and_plays() {
        let printer = Printer { incoming: VecDeque::new(), sent: Vec::new(), pending: Vec::new(), code_ok: true };
        let mut rtsp = Rtsp::new(printer, "rtsps://printer:322/streaming/live/1".into(), &Secret::new(b"TESTCODE".to_vec()));
        let track = rtsp.start().unwrap();
        let mut d = Depacketizer::new(track.parameter_sets.clone());
        let mut units = Vec::new();
        loop {
            match rtsp.next().unwrap() {
                Incoming::Response(r) => assert_eq!(r.code, 200),
                Incoming::Rtp { channel, packet } => { assert_eq!(channel, 0); units.extend(d.push(&packet)); break; }
            }
        }
        assert_eq!(units[0].annex_b, vec![0, 0, 0, 1, 0x41, 0x99]);
        let printer = rtsp.teardown();
        assert_eq!(printer.sent, ["OPTIONS", "DESCRIBE", "DESCRIBE+auth", "SETUP+auth", "PLAY+auth", "TEARDOWN+auth"]);
        let wrong = Printer { incoming: VecDeque::new(), sent: Vec::new(), pending: Vec::new(), code_ok: false };
        let mut rtsp = Rtsp::new(wrong, "rtsps://printer:322/streaming/live/1".into(), &Secret::new(b"WRONGCOD".to_vec()));
        assert_eq!(rtsp.start().unwrap_err(), CameraError::Status { request: "DESCRIBE", code: 401 });
        assert!(CameraError::Status { request: "DESCRIBE", code: 401 }.message().contains("access code"));
    }
}

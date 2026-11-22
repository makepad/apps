use crate::secrets::Secret;
use makepad_strict_json::{obj, s, Value};
use std::hash::{BuildHasher, Hasher, RandomState};
use std::io::{self, Read, Write};
use std::sync::{atomic::{AtomicU64, Ordering}, OnceLock};
use std::time::{Duration, Instant};

pub(crate) trait ByteStream: Read + Write {}
impl<T: Read + Write> ByteStream for T {}

/// The start-print command (`print.project_file`). Field set from the transport research;
/// it is provisional until a reference capture of the printer's own client confirms it.
/// Only the crate makes one, from a [`crate::PrintJob`] (so from a validated file).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ProjectFile {
    /// The uploaded file at the storage root.
    pub(crate) file: String,
    /// 1-based plate number inside the package.
    pub(crate) plate: u32,
    /// Uppercase hex MD5 of the uploaded file.
    pub(crate) md5: String,
    /// The job name shown on the printer.
    pub(crate) name: String,
    /// Per project filament: global AMS tray index, -1 unmapped or external.
    pub(crate) ams_mapping: Vec<i32>,
    /// Per project filament: (ams id, slot id); (255, 255) unmapped.
    pub(crate) ams_mapping2: Vec<(u8, u8)>,
    pub(crate) use_ams: bool,
    pub(crate) bed_leveling: bool,
    pub(crate) flow_calibration: bool,
    pub(crate) vibration_calibration: bool,
    pub(crate) layer_inspect: bool,
    pub(crate) timelapse: bool,
}

impl ProjectFile {
    fn fields(&self) -> Vec<(&'static str, Value)> {
        let flag = |b: bool| Value::Bool(b);
        let tri = |b: bool| Value::Int(b as i64);
        vec![
            ("param", s(format!("Metadata/plate_{}.gcode", self.plate))),
            ("url", s(format!("ftp:///{}", self.file))),
            ("file", s(&self.file)),
            ("md5", s(&self.md5)),
            ("subtask_name", s(&self.name)),
            ("project_id", s("0")), ("profile_id", s("0")), ("task_id", s("0")), ("subtask_id", s("0")),
            ("plate_idx", Value::Int(self.plate as i64)),
            ("use_ams", flag(self.use_ams)),
            ("ams_mapping", Value::Arr(self.ams_mapping.iter().map(|n| Value::Int(*n as i64)).collect())),
            ("ams_mapping2", Value::Arr(self.ams_mapping2.iter().map(|(a, t)| obj(vec![("ams_id", Value::Int(*a as i64)), ("slot_id", Value::Int(*t as i64))])).collect())),
            ("bed_type", s("auto")),
            ("timelapse", flag(self.timelapse)),
            ("bed_levelling", flag(self.bed_leveling)),
            ("flow_cali", flag(self.flow_calibration)),
            ("vibration_cali", flag(self.vibration_calibration)),
            ("layer_inspect", flag(self.layer_inspect)),
            ("auto_bed_leveling", tri(self.bed_leveling)),
            ("auto_flow_cali", tri(self.flow_calibration)),
            ("auto_offset_cali", Value::Int(0)),
            ("extrude_cali_manual_mode", Value::Int(-1)),
            ("nozzle_mapping", Value::Arr(Vec::new())),
            ("nozzles_info", Value::Arr(Vec::new())),
        ]
    }
}

/// Every request the link can publish. There is no raw G-code, motion,
/// temperature or settings command.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    PushAll,
    GetVersion,
    StartPrint(ProjectFile),
    Pause,
    Resume,
    Stop,
    /// Clears the error the printer shows (`print_error`) once it is understood.
    CleanPrintError { print_error: u32 },
}

impl Request {
    /// The command name its acknowledgement echoes.
    pub(crate) fn command(&self) -> &'static str {
        match self {
            Self::PushAll => "pushall", Self::GetVersion => "get_version", Self::StartPrint(_) => "project_file",
            Self::Pause => "pause", Self::Resume => "resume", Self::Stop => "stop", Self::CleanPrintError { .. } => "clean_print_error",
        }
    }

    /// Control commands go with QoS 1; the two queries with QoS 0.
    fn control(&self) -> bool { !matches!(self, Self::PushAll | Self::GetVersion) }

    fn to_json(&self, sequence: u64) -> String {
        let seq = ("sequence_id", s(sequence.to_string()));
        let (module, mut fields) = match self {
            Self::PushAll => ("pushing", vec![seq, ("command", s("pushall")), ("version", Value::Int(1)), ("push_target", Value::Int(1))]),
            Self::GetVersion => ("info", vec![seq, ("command", s("get_version"))]),
            Self::StartPrint(p) => ("print", { let mut f = vec![seq, ("command", s("project_file"))]; f.extend(p.fields()); f }),
            Self::Pause | Self::Resume | Self::Stop => ("print", vec![seq, ("command", s(self.command())), ("param", s(""))]),
            Self::CleanPrintError { print_error } => ("print", vec![seq, ("command", s("clean_print_error")), ("subtask_id", s("")), ("print_error", Value::Int(*print_error as i64))]),
        };
        fields.shrink_to_fit();
        obj(vec![(module, obj(fields))]).to_json()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Error {
    WrongAccessCode,
    Protocol,
    Transport,
    Timeout,
    Rejected,
}

pub(crate) enum Event { Idle, Ready, Report(Vec<u8>) }

/// MQTT's four-byte remaining length is a wire-format bound, not a report cap.
fn encode_remaining(mut length: usize) -> Result<Vec<u8>, Error> {
    if length > 268_435_455 { return Err(Error::Protocol); }
    let mut out = Vec::new();
    loop {
        let mut byte = (length % 128) as u8;
        length /= 128;
        if length != 0 { byte |= 128; }
        out.push(byte);
        if length == 0 { return Ok(out); }
    }
}

fn decode_remaining(bytes: &[u8]) -> Result<Option<(usize, usize)>, Error> {
    let mut length = 0usize;
    for (i, b) in bytes.iter().take(4).enumerate() {
        length |= ((b & 127) as usize) << (7 * i);
        if b & 128 == 0 {
            if i > 0 && *b == 0 { return Err(Error::Protocol); }
            return Ok(Some((length, i + 1)));
        }
    }
    if bytes.len() >= 4 { Err(Error::Protocol) } else { Ok(None) }
}

fn string(out: &mut Vec<u8>, text: &[u8]) -> Result<(), Error> {
    let length = u16::try_from(text.len()).map_err(|_| Error::Protocol)?;
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(text);
    Ok(())
}

// The raw framing writer stays private. Session's only publish input is the enum.
fn send(stream: &mut impl ByteStream, header: u8, body: &[u8]) -> Result<(), Error> {
    stream.write_all(&[header]).map_err(|_| Error::Transport)?;
    stream.write_all(&encode_remaining(body.len())?).map_err(|_| Error::Transport)?;
    stream.write_all(body).map_err(|_| Error::Transport)?;
    stream.flush().map_err(|_| Error::Transport)
}

#[derive(Default)]
struct Decoder { bytes: Vec<u8>, consumed: usize }

impl Decoder {
    fn next(&mut self) -> Result<Option<(u8, Vec<u8>)>, Error> {
        let pending = &self.bytes[self.consumed..];
        let Some(&header) = pending.first() else { return Ok(None) };
        let Some((length, count)) = decode_remaining(&pending[1..])? else { return Ok(None) };
        let start = 1 + count;
        let end = start + length;
        let Some(body) = pending.get(start..end) else { return Ok(None) };
        let result = (header, body.to_vec());
        self.consumed += end;
        if self.consumed == self.bytes.len() { self.bytes.clear(); self.consumed = 0; }
        Ok(Some(result))
    }

    fn read(&mut self, stream: &mut impl ByteStream) -> Result<(), Error> {
        let mut buffer = [0u8; 8192];
        match stream.read(&mut buffer) {
            Ok(0) => Err(Error::Transport),
            Ok(n) => {
                if self.consumed != 0 { self.bytes.drain(..self.consumed); self.consumed = 0; }
                self.bytes.extend_from_slice(&buffer[..n]);
                Ok(())
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => Ok(()),
            Err(_) => Err(Error::Transport),
        }
    }
}

/// The sequence ids: only these five digits are proven on a real printer. The id travels in the
/// command itself, so firmware that refused a longer one would refuse a stop on a live print, which
/// is far worse than a rare clash with another client's answer. Widening this is a separate decision,
/// once the `fab printer:` log shows how real printers treat ids.
const FIRST_ID: u64 = 70000;
const ID_COUNT: u64 = 10000;

/// The id for a place in the range, which it wraps inside.
fn sequence_id(place: u64) -> u64 { FIRST_ID + place % ID_COUNT }

/// The next sequence id. One counter for every connection: an answer that arrives late, or over
/// the next connection, only matches the command it belongs to until the ids come round again,
/// ten thousand commands later. Each process starts at a random place, because the printer sends
/// every client every answer and the echoed id is all that says whose an answer is. That makes
/// meeting another client's id unlikely, not impossible.
fn next_sequence() -> u64 {
    static PLACE: OnceLock<AtomicU64> = OnceLock::new();
    sequence_id(PLACE.get_or_init(|| AtomicU64::new(RandomState::new().build_hasher().finish() % ID_COUNT)).fetch_add(1, Ordering::Relaxed))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase { ConnAck, SubAck, Ready }

pub(crate) struct Session<S: ByteStream> {
    stream: S,
    serial: String,
    decoder: Decoder,
    report_topic: String,
    request_topic: String,
    phase: Phase,
    started: Instant,
    sent: Instant,
    ping: Option<Instant>,
    packet_id: u16,
}

impl<S: ByteStream> Session<S> {
    pub(crate) fn connect(mut stream: S, serial: &str, code: &Secret, now: Instant) -> Result<Self, Error> {
        if serial.is_empty() || !serial.bytes().all(|b| b.is_ascii_alphanumeric()) { return Err(Error::Protocol); }
        let mut body = Vec::new();
        string(&mut body, b"MQTT")?;
        body.extend_from_slice(&[4, 0xc2, 0, 60]); // 3.1.1, username/password, clean session
        string(&mut body, format!("fab-{}", std::process::id()).as_bytes())?;
        string(&mut body, b"bblp")?;
        // Validate before copying the password so all subsequent paths wipe it.
        u16::try_from(code.bytes().len()).map_err(|_| Error::Protocol)?;
        string(&mut body, code.bytes())?;
        let body = Secret::new(body);
        send(&mut stream, 0x10, body.bytes())?;
        Ok(Self {
            stream, serial: serial.to_string(), decoder: Decoder::default(), report_topic: format!("device/{serial}/report"), request_topic: format!("device/{serial}/request"),
            phase: Phase::ConnAck, started: now, sent: now, ping: None, packet_id: 1,
        })
    }

    /// Subscribed: requests can go out.
    pub(crate) fn ready(&self) -> bool { self.phase == Phase::Ready }

    /// The printer's serial number, which no log line carries.
    pub(crate) fn serial(&self) -> &str { &self.serial }

    /// Seconds since this connection began, for the log.
    pub(crate) fn age(&self, now: Instant) -> f64 { now.duration_since(self.started).as_secs_f64() }

    /// Publishes `request`; returns the sequence id its acknowledgement echoes.
    pub(crate) fn publish(&mut self, request: &Request, now: Instant) -> Result<u64, Error> {
        if self.phase != Phase::Ready { return Err(Error::Protocol); }
        let sequence = next_sequence();
        let mut body = Vec::new();
        string(&mut body, self.request_topic.as_bytes())?;
        let header = if request.control() {
            self.packet_id = self.packet_id.checked_add(1).filter(|n| *n != 0).unwrap_or(2);
            body.extend_from_slice(&self.packet_id.to_be_bytes());
            0x32
        } else { 0x30 };
        body.extend_from_slice(request.to_json(sequence).as_bytes());
        send(&mut self.stream, header, &body)?;
        self.sent = now;
        // With the answers link.rs logs: what a real printer answers, and how soon. A status
        // request's answer is the status report, which is not logged.
        if *request != Request::PushAll { makepad_network::makepad_error_log::log!("fab printer: +{:.3} s sent {} id {sequence}", self.age(now), request.command()); }
        Ok(sequence)
    }

    pub(crate) fn poll(&mut self, now: Instant) -> Result<Event, Error> {
        if self.phase != Phase::Ready && now.duration_since(self.started) >= Duration::from_secs(10) { return Err(Error::Timeout); }
        if self.ping.is_some_and(|at| now.duration_since(at) >= Duration::from_secs(10)) { return Err(Error::Timeout); }
        if self.phase == Phase::Ready && self.ping.is_none() && now.duration_since(self.sent) >= Duration::from_secs(60) {
            send(&mut self.stream, 0xc0, &[])?;
            self.sent = now;
            self.ping = Some(now);
        }
        let packet = match self.decoder.next()? {
            Some(p) => Some(p),
            None => { self.decoder.read(&mut self.stream)?; self.decoder.next()? }
        };
        let Some((header, body)) = packet else { return Ok(Event::Idle) };
        match header {
            0x20 if self.phase == Phase::ConnAck && body.len() == 2 && body[0] == 0 => {
                match body[1] { 0 => {}, 4 | 5 => return Err(Error::WrongAccessCode), _ => return Err(Error::Rejected) }
                let mut subscribe = vec![0, 1];
                string(&mut subscribe, self.report_topic.as_bytes())?;
                subscribe.push(0);
                send(&mut self.stream, 0x82, &subscribe)?;
                self.sent = now;
                self.phase = Phase::SubAck;
                Ok(Event::Idle)
            }
            0x90 if self.phase == Phase::SubAck && body == [0, 1, 0] => {
                self.phase = Phase::Ready;
                self.publish(&Request::PushAll, now)?;
                self.publish(&Request::GetVersion, now)?;
                Ok(Event::Ready)
            }
            0x30 | 0x31 if self.phase != Phase::ConnAck && body.len() >= 2 => {
                let length = u16::from_be_bytes([body[0], body[1]]) as usize;
                let topic = body.get(2..2 + length).ok_or(Error::Protocol)?;
                if topic != self.report_topic.as_bytes() { return Err(Error::Protocol); }
                Ok(Event::Report(body[2 + length..].to_vec()))
            }
            0xd0 if body.is_empty() && self.phase == Phase::Ready => { self.ping = None; Ok(Event::Idle) }
            // PUBACK of a control command; the printer's own acknowledgement follows as a report.
            0x40 if body.len() == 2 && self.phase == Phase::Ready => Ok(Event::Idle),
            _ => Err(Error::Protocol),
        }
    }

    pub(crate) fn disconnect(&mut self) -> Result<(), Error> { send(&mut self.stream, 0xe0, &[]) }
}

/// The printer's end of a connection, in memory: the simulated printer's, and the link's tests'.
pub(crate) mod fake {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    pub(crate) const SERIAL: &str = "SIMULATED0001";

    #[derive(Default)]
    struct Wire { from_client: Decoder, to_client: VecDeque<u8>, published: Vec<String>, dead: bool, writes_fail: bool }

    impl Wire {
        fn say(&mut self, header: u8, body: &[u8]) {
            self.to_client.push_back(header);
            self.to_client.extend(encode_remaining(body.len()).unwrap());
            self.to_client.extend(body);
        }
    }

    /// Answers the handshake, keeps what it is sent, says what a test tells it to.
    #[derive(Clone, Default)]
    pub(crate) struct FakePrinter(Rc<RefCell<Wire>>);

    impl FakePrinter {
        pub(crate) fn report(&self, json: &str) {
            let mut body = Vec::new();
            string(&mut body, format!("device/{SERIAL}/report").as_bytes()).unwrap();
            body.extend_from_slice(json.as_bytes());
            self.0.borrow_mut().say(0x30, &body);
        }
        /// Every request's JSON so far.
        pub(crate) fn published(&self) -> Vec<String> { self.0.borrow().published.clone() }
        /// The connection breaks: reads end, writes fail.
        pub(crate) fn drop_link(&self) { self.0.borrow_mut().dead = true; }
        /// Half of it breaks: writes fail, reads go on.
        pub(crate) fn break_writes(&self) { self.0.borrow_mut().writes_fail = true; }
    }

    impl Read for FakePrinter {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let mut wire = self.0.borrow_mut();
            if wire.dead { return Ok(0); }
            if wire.to_client.is_empty() { return Err(io::ErrorKind::WouldBlock.into()); }
            let n = out.len().min(wire.to_client.len());
            for b in out.iter_mut().take(n) { *b = wire.to_client.pop_front().unwrap(); }
            Ok(n)
        }
    }

    impl Write for FakePrinter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let mut wire = self.0.borrow_mut();
            if wire.dead || wire.writes_fail { return Err(io::ErrorKind::BrokenPipe.into()); }
            wire.from_client.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            let mut wire = self.0.borrow_mut();
            if wire.dead || wire.writes_fail { return Err(io::ErrorKind::BrokenPipe.into()); }
            while let Some((header, body)) = wire.from_client.next().unwrap() {
                match header {
                    0x10 => wire.say(0x20, &[0, 0]),
                    0x82 => wire.say(0x90, &[0, 1, 0]),
                    0x30 | 0x32 => {
                        let mut at = 2 + u16::from_be_bytes([body[0], body[1]]) as usize;
                        if header == 0x32 {
                            let id = [body[at], body[at + 1]];
                            at += 2;
                            wire.say(0x40, &id);
                        }
                        let json = String::from_utf8(body[at..].to_vec()).unwrap();
                        wire.published.push(json);
                    }
                    0xc0 => wire.say(0xd0, &[]),
                    _ => {}
                }
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use makepad_strict_json::Value;
    use std::collections::VecDeque;

    #[test]
    fn varints_and_fragmented_frames_round_trip() {
        for n in [0, 1, 127, 128, 16383, 16384, 2097151, 2097152, 268435455] {
            let bytes = encode_remaining(n).unwrap();
            assert_eq!(decode_remaining(&bytes), Ok(Some((n, bytes.len()))));
            for i in 0..bytes.len() { assert_eq!(decode_remaining(&bytes[..i]), Ok(None)); }
        }
        assert_eq!(encode_remaining(268435456), Err(Error::Protocol));
        assert_eq!(decode_remaining(&[128, 128, 128, 128]), Err(Error::Protocol));
        assert_eq!(decode_remaining(&[128, 0]), Err(Error::Protocol));
        let payload = vec![b'x'; 16384];
        let mut decoder = Decoder::default();
        let mut pipe = io::Cursor::new(Vec::new());
        send(&mut pipe, 0x30, &payload).unwrap();
        let wire = pipe.into_inner();
        for (i, byte) in wire.iter().enumerate() {
            decoder.bytes.push(*byte);
            if i + 1 < wire.len() { assert_eq!(decoder.next(), Ok(None)); }
        }
        assert_eq!(decoder.next(), Ok(Some((0x30, payload))));
        assert_eq!(decoder.next(), Ok(None));
    }

    fn sample_job() -> ProjectFile {
        use crate::model::TrayId;
        let job = crate::PrintJob {
            job: crate::safety::tests::checked_square("fab \"plate\""),
            trays: vec![Some(TrayId { ams: 0, slot: 0 }), Some(TrayId { ams: 1, slot: 1 }), None], used: vec![0], options: Default::default(),
        };
        crate::job::project_file(&job, "fab-plate.gcode.3mf")
    }

    #[test]
    fn every_request_is_one_module_envelope_with_its_sequence() {
        use makepad_strict_json::parse;
        let cases = [
            (Request::PushAll, "pushing", "pushall"), (Request::GetVersion, "info", "get_version"),
            (Request::StartPrint(sample_job()), "print", "project_file"), (Request::Pause, "print", "pause"),
            (Request::Resume, "print", "resume"), (Request::Stop, "print", "stop"),
            (Request::CleanPrintError { print_error: 0x0300800D }, "print", "clean_print_error"),
        ];
        for (request, module, command) in cases {
            let value = parse(request.to_json(70001).as_bytes()).unwrap();
            let Value::Obj(fields) = &value else { panic!("request object") };
            assert_eq!(fields.len(), 1);
            let fields = value.get(module).unwrap();
            assert_eq!(fields.get("command").and_then(Value::as_str), Some(command));
            assert_eq!(request.command(), command);
            assert_eq!(fields.get("sequence_id").and_then(Value::as_str), Some("70001"));
            assert_eq!(fields.get("version").and_then(Value::as_i64), (request == Request::PushAll).then_some(1));
            assert_eq!(request.control(), module == "print");
        }
        let job = parse(Request::StartPrint(sample_job()).to_json(70002).as_bytes()).unwrap();
        let p = job.get("print").unwrap();
        assert_eq!(p.get("param").and_then(Value::as_str), Some("Metadata/plate_1.gcode"));
        assert_eq!(p.get("url").and_then(Value::as_str), Some("ftp:///fab-plate.gcode.3mf"));
        assert_eq!(p.get("subtask_name").and_then(Value::as_str), Some("fab \"plate\""), "names are JSON-escaped");
        assert_eq!(p.get("ams_mapping").unwrap().to_json(), "[0,5,-1]");
        assert_eq!(p.get("md5").and_then(Value::as_str).map(str::len), Some(32));
        assert_eq!(p.get("ams_mapping2").and_then(Value::as_arr).unwrap()[2].get("slot_id").and_then(Value::as_i64), Some(255));
        assert_eq!(p.get("bed_levelling"), Some(&Value::Bool(true)));
        assert_eq!(p.get("timelapse"), Some(&Value::Bool(false)));
    }

    /// A duplex memory pipe. The broker consumes complete client frames on flush,
    /// then supplies one byte per read with timeouts between bytes.
    struct Broker {
        outgoing: Decoder,
        incoming: VecDeque<u8>,
        seen: Vec<u8>,
        reject: bool,
        timeout: bool,
        /// The sequence id of each request, in order.
        ids: Vec<u64>,
    }

    fn sequence_of(json: &[u8]) -> u64 {
        let request = makepad_strict_json::parse(json).unwrap();
        let Value::Obj(modules) = &request else { panic!("request object") };
        modules[0].1.get("sequence_id").and_then(Value::as_str).unwrap().parse().unwrap()
    }

    impl Broker {
        fn new(reject: bool) -> Self {
            Self { outgoing: Decoder::default(), incoming: VecDeque::new(), seen: Vec::new(), reject, timeout: false, ids: Vec::new() }
        }
        fn reply(&mut self, header: u8, body: &[u8]) {
            self.incoming.push_back(header);
            self.incoming.extend(encode_remaining(body.len()).unwrap());
            self.incoming.extend(body);
        }
    }
    impl Read for Broker {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.timeout = !self.timeout;
            if self.timeout { return Err(io::ErrorKind::TimedOut.into()); }
            if out.is_empty() { return Ok(0); }
            match self.incoming.pop_front() {
                Some(byte) => { out[0] = byte; Ok(1) }
                None => Err(io::ErrorKind::WouldBlock.into()),
            }
        }
    }
    impl Write for Broker {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.outgoing.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            while let Some((header, body)) = self.outgoing.next().unwrap() {
                self.seen.push(header);
                match header {
                    0x10 => {
                        assert_eq!(&body[..10], b"\0\x04MQTT\x04\xc2\0\x3c");
                        assert!(body.ends_with(b"\0\x04bblp\0\x08TESTCODE"));
                        self.reply(0x20, &[0, if self.reject { 5 } else { 0 }]);
                    }
                    0x82 => {
                        let mut expected = vec![0, 1];
                        string(&mut expected, b"device/TESTSERIAL0001/report").unwrap();
                        expected.push(0);
                        assert_eq!(body, expected);
                        self.reply(0x90, &[0, 1, 0]);
                    }
                    0x30 => {
                        let len = u16::from_be_bytes([body[0], body[1]]) as usize;
                        assert_eq!(&body[2..2 + len], b"device/TESTSERIAL0001/request");
                        let expected = [Request::PushAll, Request::GetVersion][self.ids.len()].clone();
                        let id = sequence_of(&body[2 + len..]);
                        assert_eq!(&body[2 + len..], expected.to_json(id).as_bytes());
                        self.ids.push(id);
                        if self.ids.len() == 2 {
                            let mut report = Vec::new();
                            string(&mut report, b"device/TESTSERIAL0001/report").unwrap();
                            report.extend_from_slice(br#"{"print":{"gcode_state":"IDLE"}}"#);
                            self.reply(0x30, &report);
                        }
                    }
                    0x32 => {
                        let len = u16::from_be_bytes([body[0], body[1]]) as usize;
                        assert_eq!(&body[2 + len..4 + len], &[0, 2], "first control packet id");
                        let id = sequence_of(&body[4 + len..]);
                        assert!(body[4 + len..].starts_with(format!(r#"{{"print":{{"sequence_id":"{id}","command":"pause""#).as_bytes()));
                        self.ids.push(id);
                        self.reply(0x40, &[0, 2]);
                        let mut report = Vec::new();
                        string(&mut report, b"device/TESTSERIAL0001/report").unwrap();
                        report.extend_from_slice(format!(r#"{{"print":{{"command":"pause","sequence_id":"{id}","result":"success"}}}}"#).as_bytes());
                        self.reply(0x30, &report);
                    }
                    0xc0 => { assert!(body.is_empty()); self.reply(0xd0, &[]); }
                    0xe0 => assert!(body.is_empty()),
                    _ => panic!("unexpected MQTT packet kind"),
                }
            }
            Ok(())
        }
    }

    #[test]
    fn session_over_memory_pipe_subscribes_queries_reports_pings_and_disconnects() {
        let now = Instant::now();
        let mut session = Session::connect(Broker::new(false), "TESTSERIAL0001", &Secret::new(b"TESTCODE".to_vec()), now).unwrap();
        let mut ready = false;
        let mut reports = 0;
        // Each poll consumes one byte or a timeout; the fake broker has finite data.
        while !session.stream.incoming.is_empty() {
            match session.poll(now).unwrap() {
                Event::Ready => ready = true,
                Event::Report(payload) => {
                    reports += 1;
                    assert!(ready);
                    assert_eq!(payload.as_slice(), br#"{"print":{"gcode_state":"IDLE"}}"#);
                }
                Event::Idle => {}
            }
        }
        assert!(ready);
        assert_eq!(reports, 1);
        assert_eq!(session.stream.ids.len(), 2);
        let ping_at = now + Duration::from_secs(60);
        session.poll(ping_at).unwrap();
        while !session.stream.incoming.is_empty() { session.poll(ping_at).unwrap(); }
        assert!(session.ping.is_none());
        let pause = session.publish(&Request::Pause, ping_at).unwrap();
        assert_eq!(session.stream.ids[2], pause, "publish returns the id that went out");
        let mut acks = Vec::new();
        while !session.stream.incoming.is_empty() {
            if let Event::Report(payload) = session.poll(ping_at).unwrap() { acks.push(crate::model::ack(&payload).unwrap()); }
        }
        assert_eq!(acks, [crate::model::Ack { command: "pause".into(), sequence: pause, ok: Some(true), reason: None }]);
        session.disconnect().unwrap();
        assert_eq!(session.stream.seen, [0x10, 0x82, 0x30, 0x30, 0xc0, 0x32, 0xe0]);
    }

    #[test]
    fn no_two_connections_share_a_sequence_id() {
        let now = Instant::now();
        let connected = || {
            let mut session = Session::connect(Broker::new(false), "TESTSERIAL0001", &Secret::new(b"TESTCODE".to_vec()), now).unwrap();
            while !session.ready() { session.poll(now).unwrap(); }
            session
        };
        let (mut one, mut two) = (connected(), connected());
        one.publish(&Request::Pause, now).unwrap();
        two.publish(&Request::Pause, now).unwrap();
        let mut ids = one.stream.ids.clone();
        ids.extend(&two.stream.ids);
        let count = ids.len();
        assert_eq!(count, 6);
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), count, "{ids:?}");
    }

    #[test]
    fn every_id_that_goes_out_is_in_the_range_proven_on_a_real_printer() {
        let now = Instant::now();
        let mut session = Session::connect(Broker::new(false), "TESTSERIAL0001", &Secret::new(b"TESTCODE".to_vec()), now).unwrap();
        while !session.ready() { session.poll(now).unwrap(); }
        session.publish(&Request::Pause, now).unwrap();
        assert_eq!(session.stream.ids.len(), 3);
        assert!(session.stream.ids.iter().all(|id| (70000..=79999).contains(id)), "{:?}", session.stream.ids);
    }

    #[test]
    fn ids_wrap_inside_the_range() {
        assert_eq!(sequence_id(0), 70000);
        assert_eq!(sequence_id(9999), 79999);
        assert_eq!(sequence_id(10000), 70000, "the next one after the last");
        assert_eq!(sequence_id(u64::MAX), 71615);
    }

    #[test]
    fn connack_five_stops_before_subscribe_or_publish() {
        let now = Instant::now();
        let mut session = Session::connect(Broker::new(true), "TESTSERIAL0001", &Secret::new(b"TESTCODE".to_vec()), now).unwrap();
        let error = loop {
            match session.poll(now) { Err(e) => break e, Ok(_) => assert!(!session.stream.incoming.is_empty()) }
        };
        assert_eq!(error, Error::WrongAccessCode);
        assert_eq!(session.stream.seen, [0x10]);
    }
}

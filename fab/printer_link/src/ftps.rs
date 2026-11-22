//! Implicit-TLS FTP (port 990) to the printer's storage: login, listing,
//! size, upload and delete. Every data channel resumes the control
//! connection's TLS session (the printer refuses one that does not).
//!
//! Order of a transfer: EPSV (or PASV) for the port, the transfer command
//! written, then the data channel dialled. The server accepts the data
//! connection only while it serves the command, so the TLS handshake of a
//! channel dialled before the command would wait for nothing. The data
//! address is always the configured host, never the one a PASV reply names.

use crate::safety::ValidatedJob;
use crate::secrets::Secret;
use std::io::{self, Read, Write};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FtpError {
    /// Could not open or authenticate a TLS connection.
    Connect(&'static str),
    /// The server answered a command with this code (text redacted, one line).
    Reply { command: &'static str, code: u16, text: String },
    Protocol(&'static str),
    Transport,
    /// The platform cannot resume the control session on a data channel.
    ResumeUnsupported,
    Cancelled,
    SizeMismatch { local: u64, remote: Option<u64> },
}

impl FtpError {
    pub fn message(&self) -> String {
        match self {
            Self::Connect(reason) => (*reason).to_string(),
            Self::Reply { command, code: 530, .. } => format!("{command}: the printer refused the login (access code wrong or changed)"),
            Self::Reply { command, code: 550 | 553, .. } => format!("{command}: the printer storage is missing or refused the file (insert a USB drive)"),
            Self::Reply { command, code: 452 | 552, .. } => format!("{command}: the printer storage is full"),
            Self::Reply { command, code: 522, .. } => format!("{command}: the printer refused the data channel (TLS session not resumed)"),
            Self::Reply { command, code, text } => format!("{command}: printer answered {code} {text}"),
            Self::Protocol(reason) => format!("printer FTP: {reason}"),
            Self::Transport => "printer storage connection lost".into(),
            Self::ResumeUnsupported => "printing to this printer isn't supported on this platform (TLS session resumption)".into(),
            Self::Cancelled => "upload cancelled".into(),
            Self::SizeMismatch { local, remote } => match remote {
                Some(r) => format!("upload incomplete: {r} of {local} bytes on the printer"),
                None => format!("upload incomplete: the printer does not report the file ({local} bytes sent)"),
            },
        }
    }
}

/// Opens the TLS connections of one FTPS session.
pub trait Dialer {
    type Stream: Read + Write;
    /// The control connection (TLS up and the server authenticated).
    fn control(&mut self) -> Result<Self::Stream, FtpError>;
    /// A data channel on `port` of the same host, resuming `control`'s session.
    fn data(&mut self, control: &Self::Stream, port: u16) -> Result<Self::Stream, FtpError>;
    /// Ends a data channel after a transfer (TLS close_notify, then TCP close).
    fn finish(&mut self, data: Self::Stream);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply { pub code: u16, pub text: String }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry { pub name: String, pub size: Option<u64>, pub dir: bool }

/// The printer's file names: plain ASCII, no path, no control characters.
pub fn valid_file_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 200 && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// `Fab-My part 2` -> `Fab-My-part-2.gcode.3mf`.
pub fn job_file_name(stem: &str) -> String {
    let mut s: String = stem.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    while s.contains("--") { s = s.replace("--", "-"); }
    let s = s.trim_matches('-');
    let s: String = s.chars().take(64).collect();
    format!("{}.gcode.3mf", if s.is_empty() { "fab-plate" } else { &s })
}

pub struct Ftps<D: Dialer> {
    dialer: D,
    control: D::Stream,
    buffer: Vec<u8>,
    redact: Option<Secret>,
}

fn reply_code(line: &[u8]) -> Option<u16> {
    let code = line.get(..3)?;
    if !code.iter().all(u8::is_ascii_digit) { return None; }
    std::str::from_utf8(code).ok()?.parse().ok()
}

impl<D: Dialer> Ftps<D> {
    /// Connects, reads the greeting and logs in as `bblp`; protects the data channel.
    pub fn login(mut dialer: D, code: &Secret) -> Result<Self, FtpError> {
        let control = dialer.control()?;
        let mut ftp = Self { dialer, control, buffer: Vec::new(), redact: Some(Secret::new(code.bytes().to_vec())) };
        ftp.expect_reply("connect", &[220])?;
        let user = ftp.command("USER", "USER bblp")?;
        if user.code == 331 {
            let mut line = Secret::new(b"PASS ".to_vec());
            line.extend(code.bytes());
            line.extend(b"\r\n");
            ftp.write(line.bytes())?;
            ftp.expect_reply("PASS", &[230, 202])?;
        } else if user.code != 230 {
            return Err(ftp.error("USER", user));
        }
        ftp.simple("PBSZ", "PBSZ 0", &[200])?;
        ftp.simple("PROT", "PROT P", &[200])?;
        ftp.simple("TYPE", "TYPE I", &[200])?;
        Ok(ftp)
    }

    fn clean(&self, text: &str) -> String {
        let text: String = text.chars().filter(|c| !c.is_control()).take(160).collect();
        match &self.redact { Some(code) => code.redact(&text), None => text }
    }

    fn error(&self, command: &'static str, reply: Reply) -> FtpError {
        FtpError::Reply { command, code: reply.code, text: self.clean(&reply.text) }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), FtpError> {
        self.control.write_all(bytes).map_err(|_| FtpError::Transport)?;
        self.control.flush().map_err(|_| FtpError::Transport)
    }

    fn line(&mut self) -> Result<Vec<u8>, FtpError> {
        loop {
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=end).collect();
                line.pop();
                if line.last() == Some(&b'\r') { line.pop(); }
                return Ok(line);
            }
            if self.buffer.len() > 64 * 1024 { return Err(FtpError::Protocol("reply line too long")); }
            let mut chunk = [0u8; 2048];
            match self.control.read(&mut chunk) {
                Ok(0) => return Err(FtpError::Transport),
                Ok(n) => self.buffer.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return Err(FtpError::Transport),
            }
        }
    }

    /// One reply, multi-line (`123-` ... `123 `) joined.
    pub fn reply(&mut self) -> Result<Reply, FtpError> {
        let first = self.line()?;
        let code = reply_code(&first).ok_or(FtpError::Protocol("malformed reply"))?;
        let mut text = String::from_utf8_lossy(first.get(4..).unwrap_or_default()).into_owned();
        if first.get(3) == Some(&b'-') {
            loop {
                let line = self.line()?;
                if reply_code(&line) == Some(code) && line.get(3) == Some(&b' ') {
                    text.push(' ');
                    text.push_str(&String::from_utf8_lossy(&line[4..]));
                    break;
                }
            }
        }
        Ok(Reply { code, text })
    }

    fn expect_reply(&mut self, command: &'static str, ok: &[u16]) -> Result<Reply, FtpError> {
        let reply = self.reply()?;
        if ok.contains(&reply.code) { Ok(reply) } else { Err(self.error(command, reply)) }
    }

    fn command(&mut self, _name: &'static str, line: &str) -> Result<Reply, FtpError> {
        if line.bytes().any(|b| b == b'\r' || b == b'\n') { return Err(FtpError::Protocol("command contains a line break")); }
        self.write(format!("{line}\r\n").as_bytes())?;
        self.reply()
    }

    fn simple(&mut self, name: &'static str, line: &str, ok: &[u16]) -> Result<Reply, FtpError> {
        let reply = self.command(name, line)?;
        if ok.contains(&reply.code) { Ok(reply) } else { Err(self.error(name, reply)) }
    }

    /// The passive data port: EPSV, else PASV (its address is ignored).
    fn passive(&mut self) -> Result<u16, FtpError> {
        let reply = self.command("EPSV", "EPSV")?;
        if reply.code == 229 {
            return parse_epsv(&reply.text).ok_or(FtpError::Protocol("malformed EPSV reply"));
        }
        let reply = self.simple("PASV", "PASV", &[227])?;
        parse_pasv(&reply.text).ok_or(FtpError::Protocol("malformed PASV reply"))
    }

    /// Writes `line`, then dials the data channel, then reads the preliminary reply.
    fn open(&mut self, name: &'static str, line: &str) -> Result<D::Stream, FtpError> {
        let port = self.passive()?;
        if line.bytes().any(|b| b == b'\r' || b == b'\n') { return Err(FtpError::Protocol("command contains a line break")); }
        self.write(format!("{line}\r\n").as_bytes())?;
        let data = match self.dialer.data(&self.control, port) {
            Ok(d) => d,
            Err(e) => {
                // The server may have answered the command already (no storage): say that.
                return Err(match e {
                    FtpError::ResumeUnsupported => e,
                    other => match self.reply() { Ok(r) if r.code >= 400 => self.error(name, r), _ => other },
                });
            }
        };
        let reply = self.reply()?;
        if !matches!(reply.code, 125 | 150) {
            self.dialer.finish(data);
            return Err(self.error(name, reply));
        }
        Ok(data)
    }

    pub fn list(&mut self, path: &str) -> Result<Vec<Entry>, FtpError> {
        let mut data = self.open("LIST", &format!("LIST {path}"))?;
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 8192];
        let result = loop {
            match data.read(&mut chunk) {
                Ok(0) => break Ok(()),
                Ok(n) if bytes.len() + n <= 4 * 1024 * 1024 => bytes.extend_from_slice(&chunk[..n]),
                Ok(_) => break Err(FtpError::Protocol("listing too long")),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                // A TLS EOF without close_notify still ends the listing.
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break Ok(()),
                Err(_) => break Err(FtpError::Transport),
            }
        };
        self.dialer.finish(data);
        let reply = self.expect_reply("LIST", &[226, 250]);
        result?;
        reply?;
        Ok(String::from_utf8_lossy(&bytes).lines().filter_map(parse_list_line).collect())
    }

    pub fn size(&mut self, path: &str) -> Result<Option<u64>, FtpError> {
        let reply = self.command("SIZE", &format!("SIZE {path}"))?;
        match reply.code {
            213 => reply.text.trim().parse().map(Some).map_err(|_| FtpError::Protocol("malformed SIZE reply")),
            550 => Ok(None),
            _ => Err(self.error("SIZE", reply)),
        }
    }

    /// Uploads `bytes` to `path`. `progress(sent)` returns false to cancel.
    fn store(&mut self, path: &str, bytes: &[u8], progress: &mut dyn FnMut(u64) -> bool) -> Result<(), FtpError> {
        let mut data = self.open("STOR", &format!("STOR {path}"))?;
        let mut sent = 0usize;
        let result = loop {
            if sent == bytes.len() { break Ok(()); }
            if !progress(sent as u64) { break Err(FtpError::Cancelled); }
            let end = (sent + 64 * 1024).min(bytes.len());
            if data.write_all(&bytes[sent..end]).is_err() { break Err(FtpError::Transport); }
            sent = end;
        };
        let flushed = data.flush().map_err(|_| FtpError::Transport);
        self.dialer.finish(data);
        // The transfer's final reply is read on every path, so the control stays in step.
        let reply = self.expect_reply("STOR", &[226, 250]);
        result?;
        flushed?;
        reply?;
        progress(sent as u64);
        Ok(())
    }

    pub fn delete(&mut self, path: &str) -> Result<(), FtpError> {
        self.simple("DELE", &format!("DELE {path}"), &[250]).map(|_| ())
    }

    pub fn quit(mut self) {
        let _ = self.command("QUIT", "QUIT");
    }

    /// Upload a checked job as `name`, then check the size the printer reports; a short file
    /// is deleted. Only a [`ValidatedJob`] can be uploaded: there is no raw-bytes upload.
    pub fn upload(&mut self, name: &str, job: &ValidatedJob, progress: &mut dyn FnMut(u64) -> bool) -> Result<(), FtpError> {
        if !valid_file_name(name) { return Err(FtpError::Protocol("invalid job file name")); }
        if !job.intact() { return Err(FtpError::Protocol("the checked file changed")); }
        let bytes = job.bytes();
        let path = format!("/{name}");
        let stored = self.store(&path, bytes, progress);
        let remote = match stored {
            Ok(()) => self.size(&path)?,
            Err(e) => { let _ = self.delete(&path); return Err(e); }
        };
        if remote != Some(bytes.len() as u64) {
            let _ = self.delete(&path);
            return Err(FtpError::SizeMismatch { local: bytes.len() as u64, remote });
        }
        Ok(())
    }
}

/// `Entering Extended Passive Mode (|||50029|)`.
pub fn parse_epsv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = text[open..].find(')')? + open;
    let inner = &text[open + 1..close];
    let delim = inner.chars().next()?;
    let parts: Vec<_> = inner.split(delim).collect();
    if parts.len() != 5 { return None; }
    parts[3].parse().ok().filter(|p| *p != 0)
}

/// `Entering Passive Mode (10,0,0,1,195,149).` -> 50069 (the address is not used).
pub fn parse_pasv(text: &str) -> Option<u16> {
    let open = text.find('(')?;
    let close = text[open..].find(')')? + open;
    let n: Vec<u16> = text[open + 1..close].split(',').map(|s| s.trim().parse().ok()).collect::<Option<_>>()?;
    if n.len() != 6 || n.iter().any(|v| *v > 255) { return None; }
    Some(n[4] * 256 + n[5]).filter(|p| *p != 0)
}

/// One `ls -l` style line: `-rw-r--r-- 1 ftp ftp 1234 Oct 29 10:00 name`.
pub fn parse_list_line(line: &str) -> Option<Entry> {
    let line = line.trim_end();
    let first = line.chars().next()?;
    if !matches!(first, '-' | 'd' | 'l') { return None; }
    let mut rest = line;
    let mut fields = Vec::new();
    for _ in 0..8 {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace)?;
        fields.push(&rest[..end]);
        rest = &rest[end..];
    }
    let name = rest.trim_start();
    if name.is_empty() || name == "." || name == ".." { return None; }
    let name = if first == 'l' { name.split(" -> ").next().unwrap_or(name) } else { name };
    Some(Entry { name: name.to_string(), size: fields[4].parse().ok(), dir: first == 'd' })
}

/// The real dialer: implicit TLS on 990, every connection's leaf certificate
/// must be the printer's pinned one (pinned and CA-checked on the MQTT port).
#[cfg(not(target_arch = "wasm32"))]
pub struct PrinterDialer { pub host: String, pub leaf_pin: [u8; 32] }

#[cfg(not(target_arch = "wasm32"))]
impl Dialer for PrinterDialer {
    type Stream = makepad_network::SocketStream;

    fn control(&mut self) -> Result<Self::Stream, FtpError> {
        let (mut stream, leaf) = makepad_network::SocketStream::connect_capture(&self.host, "990")
            .map_err(|_| FtpError::Connect("printer storage (FTPS) connection failed"))?;
        if makepad_network::tls::fingerprint(&leaf) != self.leaf_pin {
            stream.shutdown();
            return Err(FtpError::Connect("printer storage certificate is not the pinned printer certificate"));
        }
        set_timeouts(&stream)?;
        Ok(stream)
    }

    fn data(&mut self, control: &Self::Stream, port: u16) -> Result<Self::Stream, FtpError> {
        let (mut stream, leaf) = makepad_network::SocketStream::connect_capture_resuming(&self.host, &port.to_string(), control)
            .map_err(|e| if e.kind() == io::ErrorKind::Unsupported { FtpError::ResumeUnsupported } else { FtpError::Connect("printer storage data channel failed") })?;
        if makepad_network::tls::fingerprint(&leaf) != self.leaf_pin {
            stream.shutdown();
            return Err(FtpError::Connect("printer storage data certificate is not the pinned printer certificate"));
        }
        set_timeouts(&stream)?;
        Ok(stream)
    }

    fn finish(&mut self, mut data: Self::Stream) { data.shutdown(); }
}

#[cfg(not(target_arch = "wasm32"))]
fn set_timeouts(stream: &makepad_network::SocketStream) -> Result<(), FtpError> {
    let t = Some(std::time::Duration::from_secs(30));
    stream.set_read_timeout(t).map_err(|_| FtpError::Connect("cannot set printer storage timeout"))?;
    stream.set_write_timeout(t).map_err(|_| FtpError::Connect("cannot set printer storage timeout"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    #[derive(Default)]
    struct Server {
        files: Vec<(String, Vec<u8>)>,
        /// Commands in the order the client wrote them; DATA marks a dialled channel.
        log: Vec<String>,
        pending: Option<String>,
        storage: bool,
        resumes: bool,
        truncate: bool,
        /// Data written on the open data channel.
        upload: Vec<u8>,
        listing: Vec<u8>,
        out: VecDeque<u8>,
    }

    impl Server {
        fn say(&mut self, text: &str) { self.out.extend(text.as_bytes()); self.out.extend(b"\r\n"); }
        fn line(&mut self, line: &str) {
            let (verb, arg) = line.split_once(' ').unwrap_or((line, ""));
            self.log.push(if verb == "PASS" { "PASS ***".into() } else { line.to_string() });
            match verb {
                "USER" => self.say("331 Please specify the password."),
                "PASS" => if arg == "TESTCODE" { self.say("230 Login successful.") } else { self.say("530 Login incorrect.") },
                "PBSZ" | "PROT" => self.say("200 OK"),
                "TYPE" => self.say("200 Switching to Binary mode."),
                "EPSV" => self.say("229 Entering Extended Passive Mode (|||50029|)"),
                "LIST" | "STOR" => {
                    if !self.storage { self.say("550 Failed to open file."); return; }
                    self.pending = Some(line.to_string());
                }
                "SIZE" => match self.files.iter().find(|f| f.0 == arg) {
                    Some(f) => { let n = f.1.len(); self.say(&format!("213 {n}")) }
                    None => self.say("550 Could not get file size."),
                },
                "DELE" => { self.files.retain(|f| f.0 != arg); self.say("250 Delete operation successful.") }
                "QUIT" => self.say("221 Goodbye."),
                _ => self.say("500 Unknown command."),
            }
        }
    }

    struct Control(Rc<RefCell<Server>>, Vec<u8>);
    impl Read for Control {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            let mut s = self.0.borrow_mut();
            let n = out.len().min(s.out.len()).min(7); // small reads: replies split across reads
            if n == 0 { return Ok(0); }
            for b in out.iter_mut().take(n) { *b = s.out.pop_front().unwrap(); }
            Ok(n)
        }
    }
    impl Write for Control {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.1.extend_from_slice(bytes);
            while let Some(end) = self.1.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = self.1.drain(..=end).collect();
                let line = String::from_utf8(line).unwrap();
                self.0.borrow_mut().line(line.trim_end());
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    enum Stream { Control(Control), Data(Rc<RefCell<Server>>, usize) }
    impl Read for Stream {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            match self {
                Stream::Control(c) => c.read(out),
                Stream::Data(s, at) => {
                    let s = s.borrow();
                    let n = out.len().min(s.listing.len() - *at);
                    out[..n].copy_from_slice(&s.listing[*at..*at + n]);
                    *at += n;
                    Ok(n)
                }
            }
        }
    }
    impl Write for Stream {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            match self {
                Stream::Control(c) => c.write(bytes),
                Stream::Data(s, _) => { s.borrow_mut().upload.extend_from_slice(bytes); Ok(bytes.len()) }
            }
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }

    struct Fake(Rc<RefCell<Server>>);
    impl Dialer for Fake {
        type Stream = Stream;
        fn control(&mut self) -> Result<Stream, FtpError> {
            self.0.borrow_mut().say("220 (vsFTPd 3.0.3)");
            Ok(Stream::Control(Control(self.0.clone(), Vec::new())))
        }
        fn data(&mut self, control: &Stream, port: u16) -> Result<Stream, FtpError> {
            assert!(matches!(control, Stream::Control(_)));
            assert_eq!(port, 50029);
            let mut s = self.0.borrow_mut();
            if !s.resumes { return Err(FtpError::ResumeUnsupported); }
            // The server only accepts while it serves a transfer command.
            let Some(command) = s.pending.clone() else { return Err(FtpError::Connect("no transfer command before the data channel")) };
            s.log.push("DATA".into());
            s.upload.clear();
            s.say(if command.starts_with("LIST") { "150 Here comes the directory listing." } else { "150 Ok to send data." });
            Ok(Stream::Data(self.0.clone(), 0))
        }
        fn finish(&mut self, _data: Stream) {
            let mut s = self.0.borrow_mut();
            let command = s.pending.take().unwrap();
            if let Some(path) = command.strip_prefix("STOR ") {
                let mut bytes = std::mem::take(&mut s.upload);
                if s.truncate { bytes.truncate(bytes.len() / 2); }
                let path = path.to_string();
                s.files.retain(|f| f.0 != path);
                s.files.push((path, bytes));
                s.say("226 Transfer complete.");
            } else {
                s.say("226 Directory send OK.");
            }
        }
    }

    fn server() -> Rc<RefCell<Server>> {
        let s = Rc::new(RefCell::new(Server { storage: true, resumes: true, ..Default::default() }));
        s.borrow_mut().listing = b"-rw-r--r--    1 0        0          123456 Oct 29 10:00 old job.gcode.3mf\r\ndrwxr-xr-x    2 0        0            4096 Oct 29 09:00 timelapse\r\n".to_vec();
        s
    }

    #[test]
    fn login_list_upload_size_and_delete_follow_the_command_order() {
        let s = server();
        let mut ftp = Ftps::login(Fake(s.clone()), &Secret::new(b"TESTCODE".to_vec())).unwrap();
        let list = ftp.list("/").unwrap();
        assert_eq!(list, vec![
            Entry { name: "old job.gcode.3mf".into(), size: Some(123456), dir: false },
            Entry { name: "timelapse".into(), size: Some(4096), dir: true },
        ]);
        let job = crate::safety::tests::checked_square("plate");
        let payload = job.bytes().to_vec();
        let mut seen = Vec::new();
        ftp.upload("fab-plate.gcode.3mf", &job, &mut |n| { seen.push(n); true }).unwrap();
        assert_eq!(*seen.last().unwrap(), payload.len() as u64);
        assert!(seen.windows(2).all(|w| w[0] <= w[1]));
        assert_eq!(s.borrow().files[0], ("/fab-plate.gcode.3mf".to_string(), payload.clone()));
        ftp.delete("/fab-plate.gcode.3mf").unwrap();
        ftp.quit();
        let log = s.borrow().log.clone();
        assert_eq!(log, [
            "USER bblp", "PASS ***", "PBSZ 0", "PROT P", "TYPE I",
            "EPSV", "LIST /", "DATA",
            "EPSV", "STOR /fab-plate.gcode.3mf", "DATA", "SIZE /fab-plate.gcode.3mf",
            "DELE /fab-plate.gcode.3mf", "QUIT",
        ]);
    }

    #[test]
    fn failures_say_why_and_never_leave_a_short_file() {
        let s = server();
        assert!(matches!(Ftps::login(Fake(s.clone()), &Secret::new(b"WRONGCOD".to_vec())), Err(FtpError::Reply { code: 530, .. })));
        let mut ftp = Ftps::login(Fake(s.clone()), &Secret::new(b"TESTCODE".to_vec())).unwrap();
        s.borrow_mut().truncate = true;
        let job = crate::safety::tests::checked_square("plate");
        let n = job.bytes().len() as u64;
        let err = ftp.upload("short.gcode.3mf", &job, &mut |_| true).unwrap_err();
        assert_eq!(err, FtpError::SizeMismatch { local: n, remote: Some(n / 2) });
        assert!(s.borrow().files.is_empty(), "a short upload is deleted");
        s.borrow_mut().truncate = false;
        assert_eq!(ftp.upload("cancel.gcode.3mf", &job, &mut |_| false), Err(FtpError::Cancelled));
        s.borrow_mut().storage = false;
        let err = ftp.upload("nostorage.gcode.3mf", &job, &mut |_| true).unwrap_err();
        assert!(matches!(err, FtpError::Reply { command: "STOR", code: 550, .. }), "{err:?}");
        assert!(err.message().contains("USB drive"));
        s.borrow_mut().storage = true;
        s.borrow_mut().resumes = false;
        assert_eq!(ftp.list("/"), Err(FtpError::ResumeUnsupported));
        assert!(ftp.upload("../escape", &job, &mut |_| true).is_err());
    }

    #[test]
    fn reply_and_passive_parsers() {
        assert_eq!(parse_epsv("Entering Extended Passive Mode (|||50029|)"), Some(50029));
        assert_eq!(parse_epsv("Entering Extended Passive Mode (!!!21!)"), Some(21));
        assert_eq!(parse_epsv("(|||0|)"), None);
        assert_eq!(parse_pasv("Entering Passive Mode (10,0,0,243,195,149)."), Some(195 * 256 + 149));
        assert_eq!(parse_pasv("Entering Passive Mode (10,0,0,243,300,1)."), None);
        assert_eq!(job_file_name("My part (2)"), "My-part-2.gcode.3mf");
        assert_eq!(job_file_name("///"), "fab-plate.gcode.3mf");
        assert!(valid_file_name("My-part-2.gcode.3mf"));
        assert!(!valid_file_name(".hidden") && !valid_file_name("a/b") && !valid_file_name("a\r\nDELE x"));
        assert_eq!(parse_list_line("total 0"), None);
        assert_eq!(parse_list_line("lrwxrwxrwx 1 0 0 4 Oct 29 10:00 usb -> /mnt"), Some(Entry { name: "usb".into(), size: Some(4), dir: false }));
    }

    #[test]
    fn multi_line_replies_join() {
        let s = server();
        let mut ftp = Ftps::login(Fake(s.clone()), &Secret::new(b"TESTCODE".to_vec())).unwrap();
        s.borrow_mut().say("211-Features:\r\n EPSV\r\n PASV\r\n211 End");
        let reply = ftp.reply().unwrap();
        assert_eq!(reply.code, 211);
        assert!(reply.text.ends_with("End"));
    }
}

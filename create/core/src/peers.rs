//! This device's linked devices and towns, kept in its home across restarts.

use crate::{home::Home, home_writer::PrivateFiles};
#[cfg(test)]
use crate::home::write_private;
use crate::key::PublicKey;
use crate::link::Pin;
use makepad_network::at_rest::{protect_at_rest, unprotect_at_rest};
use makepad_network::tls::{constant_time_eq, from_hex};
use std::fmt::Write;
#[cfg(test)]
use std::fs;
use std::io::{self, ErrorKind};
use std::net::{SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};

const PURPOSE: &str = "makepad create peers";
const HEADER: &str = "v1 peers\n";
const HOME_HEADER: &str = "v1 home\n";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum How { Words, Town }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceState { Active, Blocked, Removed }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TownState { Active, Left }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Device {
    pub key: PublicKey,
    pub how: How,
    pub first: u64,
    pub last: u64,
    pub link_version: u16,
    pub state: DeviceState,
    pub checked: bool,
    pub name: String,
    /// Where this device was last reached, as this side saw it (see [`PathMemory`]). Old files
    /// have none, and such a device isn't reconnected.
    pub path: PathMemory,
    /// The person disconnected: no reconnecting until they connect again. It survives a restart.
    pub paused: bool,
    extra: Vec<String>,
}

/// What a link told this side of the device at the other end, from what it saw and never from
/// what the device claimed. They are the `addr=` and `lan=` fields of the device's record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PathMemory {
    /// The device's address on the public path of the last link that ran over one.
    pub addr: Option<SocketAddr>,
    /// Its LAN candidate, from the last link that ran over the LAN.
    pub lan: Option<SocketAddrV4>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Town {
    pub id: [u8; 32],
    pub founded: u64,
    pub state: TownState,
    extra: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum Row { Device(usize), Town(usize) }

#[derive(Clone, Debug, Default)]
struct Records {
    towns: Vec<Town>,
    devices: Vec<Device>,
    order: Vec<Row>,
}

#[derive(Debug)]
pub struct Peers {
    files: PrivateFiles,
    path: PathBuf,
    records: Records,
}

impl Peers {
    #[cfg(any(test, feature = "test-hooks"))]
    pub fn load(path: &Path) -> io::Result<Peers> {
        Self::load_in(path, PrivateFiles::default())
    }

    fn load_in(path: &Path, files: PrivateFiles) -> io::Result<Peers> {
        let stored = files.read(path)?;
        let bytes = unprotect_at_rest(PURPOSE, &stored)
            .map_err(|e| io::Error::new(ErrorKind::InvalidData, format!("cannot unprotect local/peers: {e}")))?;
        let mut lines = bytes.split_inclusive(|&b| b == b'\n');
        let header = lines.next().unwrap_or_default();
        if header != HEADER.as_bytes() {
            return Err(line_error(1, "expected v1 peers followed by a newline"));
        }
        let mut records = Records::default();
        for (index, line) in lines.enumerate() {
            let number = index + 2;
            let line = line.strip_suffix(b"\n").ok_or_else(|| line_error(number, "missing final newline"))?;
            let line = std::str::from_utf8(line).map_err(|_| line_error(number, "record is not UTF-8"))?;
            records.parse_line(line).map_err(|why| line_error(number, why))?;
        }
        Ok(Peers { files, path: path.to_owned(), records })
    }

    /// The caller holds the home lock, so another Town instance cannot create the store
    /// between this existence check and the atomic write.
    #[cfg(test)]
    pub fn create(path: &Path) -> io::Result<Peers> {
        Self::create_in(path, PrivateFiles::default())
    }

    fn create_in(path: &Path, files: PrivateFiles) -> io::Result<Peers> {
        if files.exists(path)? {
            return Err(io::Error::new(ErrorKind::AlreadyExists, "peers store already exists"));
        }
        let (bytes, _) = protect_at_rest(PURPOSE, HEADER.as_bytes())?;
        files.replace(path, bytes)?;
        Ok(Peers { files, path: path.to_owned(), records: Records::default() })
    }

    pub fn open(home: &Home) -> io::Result<Peers> {
        let path = home.peers_path();
        let record_path = home.record_path();
        let (mut record, listed) = parse_home_record(read_home_record(home, &record_path)?)
            .map_err(|why| io::Error::new(ErrorKind::InvalidData, why))?;
        if listed {
            return Self::load_in(&path, home.files()).map_err(|e| {
                if e.kind() == ErrorKind::NotFound { io::Error::new(ErrorKind::NotFound, "local/peers is missing") } else { e }
            });
        }
        let peers = match Self::load_in(&path, home.files()) {
            Err(e) if e.kind() == ErrorKind::NotFound => Self::create_in(&path, home.files())?,
            other => other?,
        };
        record.push_str("peers\n");
        home.files().replace(&record_path, record.into_bytes())?;
        Ok(peers)
    }

    /// After the person confirms, move the old store aside and record the reset in this home.
    pub fn forget(home: &Home, now: u64) -> io::Result<Peers> {
        let path = home.peers_path();
        let record_path = home.record_path();
        // Read the record before moving either file. A malformed record can be preserved and rebuilt.
        let home_record = parse_home_record(read_home_record(home, &record_path)?);
        let aside = if home.files().exists(&path)? {
            move_aside(home, &path, &format!("peers.unreadable-{now}"))?
        } else {
            "-".to_owned()
        };

        let peers = Self::create_in(&path, home.files())?;
        let (mut record, listed, home_aside) = match home_record {
            Ok((text, listed)) => (text, listed, None),
            Err(_) => {
                let name = move_aside(home, &record_path, &format!("home.unreadable-{now}"))?;
                (HOME_HEADER.to_owned(), false, Some(name))
            }
        };
        if !listed {
            record.push_str("peers\n");
        }
        writeln!(record, "forgot peers {now} {aside}").unwrap();
        if let Some(name) = home_aside {
            writeln!(record, "forgot home {now} {name}").unwrap();
        }
        home.files().replace(&record_path, record.into_bytes())?;
        Ok(peers)
    }

    pub fn devices(&self) -> &[Device] { &self.records.devices }
    pub fn towns(&self) -> &[Town] { &self.records.towns }

    pub fn device(&self, key: &PublicKey) -> Option<&Device> {
        self.records.device_index(key).map(|i| &self.devices()[i])
    }

    pub fn linked(&mut self, key: PublicKey, how: How, version: u16, name: &str, now: u64) -> io::Result<()> {
        if key[0] != 4 {
            return Err(io::Error::new(ErrorKind::InvalidInput, "device key must start with 04"));
        }
        self.change(|records| {
            if let Some(index) = records.device_index(&key) {
                let device = &mut records.devices[index];
                device.last = device.last.max(now);
                device.link_version = device.link_version.max(version);
                // The label belongs to this pin from its first contact.
            } else {
                records.push_device(Device {
                    key, how, first: now, last: now, link_version: version, state: DeviceState::Active,
                    checked: false, name: name.to_owned(), path: PathMemory::default(), paused: false, extra: Vec::new(),
                });
            }
            Ok(())
        })
    }

    pub fn set_state(&mut self, key: &PublicKey, state: DeviceState) -> io::Result<()> {
        self.change(|records| {
            records.device_mut(key)?.state = state;
            Ok(())
        })
    }

    /// Records where a link reached `key`: each address given replaces the old one, and none given
    /// keeps it. Nothing is written when nothing changes.
    pub fn remember_path(&mut self, key: &PublicKey, seen: PathMemory) -> io::Result<()> {
        let Some(device) = self.device(key) else { return Err(io::Error::new(ErrorKind::NotFound, "unknown device")) };
        let path = PathMemory { addr: seen.addr.or(device.path.addr), lan: seen.lan.or(device.path.lan) };
        if path == device.path {
            return Ok(());
        }
        self.change(|records| {
            records.device_mut(key)?.path = path;
            Ok(())
        })
    }

    /// Pauses or resumes reconnecting to `key`.
    pub fn set_paused(&mut self, key: &PublicKey, paused: bool) -> io::Result<()> {
        if self.device(key).is_some_and(|d| d.paused == paused) {
            return Ok(());
        }
        self.change(|records| {
            records.device_mut(key)?.paused = paused;
            Ok(())
        })
    }

    /// Resolve a key selected by the local fold or a local offer into a transport pin.
    /// A member learned from the log can acquire a path later; pause and block always win.
    pub fn carrier_pin(&self, key: PublicKey) -> Option<(Pin, PathMemory)> {
        match self.device(&key) {
            Some(d) if d.paused || d.state != DeviceState::Active => None,
            Some(d) => Some((Pin { device: key, min_version: d.link_version }, d.path)),
            None => Some((Pin { device: key, min_version: crate::link::VERSION }, PathMemory::default())),
        }
    }

    pub fn set_checked(&mut self, key: &PublicKey, checked: bool) -> io::Result<()> {
        self.change(|records| {
            records.device_mut(key)?.checked = checked;
            Ok(())
        })
    }

    pub fn add_town(&mut self, id: [u8; 32], founded: u64) -> io::Result<()> {
        self.change(|records| {
            if records.towns.iter().any(|r| r.id == id) {
                return Err(io::Error::new(ErrorKind::AlreadyExists, "town already exists"));
            }
            records.push_town(Town { id, founded, state: TownState::Active, extra: Vec::new() });
            Ok(())
        })
    }

    pub fn set_town_state(&mut self, id: &[u8; 32], state: TownState) -> io::Result<()> {
        self.change(|records| {
            let town = records.towns.iter_mut().find(|r| &r.id == id)
                .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "unknown town"))?;
            town.state = state;
            Ok(())
        })
    }

    pub fn pins(&self) -> Vec<Pin> {
        self.devices().iter().filter(|d| d.state == DeviceState::Active)
            .map(|d| Pin { device: d.key, min_version: d.link_version }).collect()
    }

    fn change(&mut self, edit: impl FnOnce(&mut Records) -> io::Result<()>) -> io::Result<()> {
        let mut next = self.records.clone();
        edit(&mut next)?;
        let (bytes, _) = protect_at_rest(PURPOSE, next.text().as_bytes())?;
        self.files.replace(&self.path, bytes)?;
        self.records = next;
        Ok(())
    }
}

impl Records {
    fn push_device(&mut self, device: Device) {
        self.order.push(Row::Device(self.devices.len()));
        self.devices.push(device);
    }

    fn push_town(&mut self, town: Town) {
        self.order.push(Row::Town(self.towns.len()));
        self.towns.push(town);
    }

    fn device_index(&self, key: &PublicKey) -> Option<usize> {
        self.devices.iter().position(|d| constant_time_eq(&d.key, key))
    }

    fn device_mut(&mut self, key: &PublicKey) -> io::Result<&mut Device> {
        let i = self.device_index(key).ok_or_else(|| io::Error::new(ErrorKind::NotFound, "unknown device"))?;
        Ok(&mut self.devices[i])
    }

    fn parse_line(&mut self, line: &str) -> Result<(), &'static str> {
        if line.contains('\t') { return Err("tab in record"); }
        let fields: Vec<_> = line.split(' ').collect();
        if fields.contains(&"") { return Err("empty field"); }
        match fields.as_slice() {
            ["v1", "town", id, founded, state, extra @ ..] => {
                let id = fixed_hex(id).ok_or("expected 64 hex digits for town id")?;
                if self.towns.iter().any(|r| r.id == id) { return Err("duplicate town id"); }
                let founded = number(founded, "invalid town founding time")?;
                let state = match *state {
                    "active" => TownState::Active,
                    "left" => TownState::Left,
                    _ => return Err("unknown town state"),
                };
                self.push_town(Town { id, founded, state, extra: extensions(extra)? });
            }
            ["v1", "device", key, how, first, last, version, state, checked, name, extra @ ..] => {
                let key: PublicKey = fixed_hex(key).ok_or("expected 130 hex digits for device key")?;
                if key[0] != 4 { return Err("device key must start with 04"); }
                if self.device_index(&key).is_some() { return Err("duplicate device key"); }
                let how = match *how {
                    "words" => How::Words,
                    "town" => How::Town,
                    _ => return Err("unknown device how"),
                };
                let first = number(first, "invalid first link time")?;
                let last = number(last, "invalid last link time")?;
                if last < first { return Err("latest link predates first link"); }
                let state = match *state {
                    "active" => DeviceState::Active,
                    "blocked" => DeviceState::Blocked,
                    "removed" => DeviceState::Removed,
                    _ => return Err("unknown device state"),
                };
                let checked = match *checked {
                    "checked" => true,
                    "unchecked" => false,
                    _ => return Err("unknown checked value"),
                };
                let mut extra = extensions(extra)?;
                let (path, paused) = take_path(&mut extra)?;
                self.push_device(Device {
                    key, how, first, last, link_version: number(version, "invalid link version")?, state,
                    checked, name: decode_name(name)?, path, paused, extra,
                });
            }
            ["v1", "device" | "town", ..] => return Err("too few record fields"),
            ["v1", _, ..] => return Err("unknown record kind"),
            ["v1"] => return Err("missing record fields"),
            _ => return Err("unknown record version"),
        }
        Ok(())
    }

    fn text(&self) -> String {
        let mut text = HEADER.to_owned();
        for row in &self.order {
            let extra = match *row {
                Row::Town(i) => {
                    let r = &self.towns[i];
                    let state = match r.state { TownState::Active => "active", TownState::Left => "left" };
                    write!(text, "v1 town {} {} {state}", crate::hex(&r.id), r.founded).unwrap();
                    &r.extra
                }
                Row::Device(i) => {
                    let d = &self.devices[i];
                    let how = match d.how { How::Words => "words", How::Town => "town" };
                    let state = match d.state {
                        DeviceState::Active => "active", DeviceState::Blocked => "blocked", DeviceState::Removed => "removed",
                    };
                    let checked = if d.checked { "checked" } else { "unchecked" };
                    let name = if d.name.is_empty() { "-".to_owned() } else { crate::hex(d.name.as_bytes()) };
                    write!(text, "v1 device {} {how} {} {} {} {state} {checked} {name}",
                        crate::hex(&d.key), d.first, d.last, d.link_version).unwrap();
                    if let Some(addr) = d.path.addr { write!(text, " addr={addr}").unwrap(); }
                    if let Some(lan) = d.path.lan { write!(text, " lan={lan}").unwrap(); }
                    if d.paused { text.push_str(" paused=1"); }
                    &d.extra
                }
            };
            for field in extra {
                text.push(' ');
                text.push_str(field);
            }
            text.push('\n');
        }
        text
    }
}

fn move_aside(home: &Home, path: &Path, base: &str) -> io::Result<String> {
    let mut name = base.to_owned();
    let mut suffix = 2u64;
    while home.files().exists(&path.with_file_name(&name))? {
        name = format!("{base}-{suffix}");
        suffix += 1;
    }
    home.files().rename(path, &path.with_file_name(&name))?;
    Ok(name)
}

fn read_home_record(home: &Home, path: &Path) -> io::Result<Vec<u8>> {
    match home.files().read(path) {
        Err(e) if e.kind() == ErrorKind::NotFound && !home.files().exists(path)? => Ok(HOME_HEADER.as_bytes().to_vec()),
        result => result,
    }
}

/// The record ready to append to, and whether it already lists the store.
fn parse_home_record(bytes: Vec<u8>) -> Result<(String, bool), &'static str> {
    let mut record = String::from_utf8(bytes).map_err(|_| "local/home is not UTF-8")?;
    let mut lines = record.lines();
    if lines.next() != Some("v1 home") {
        return Err("local/home line 1: expected v1 home");
    }
    let listed = lines.any(|line| line == "peers");
    if !record.ends_with('\n') {
        record.push('\n');
    }
    Ok((record, listed))
}

fn line_error(number: usize, why: &str) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, format!("local/peers line {number}: {why}"))
}

fn number<T: std::str::FromStr>(s: &str, why: &'static str) -> Result<T, &'static str> {
    if !s.bytes().all(|b| b.is_ascii_digit()) { return Err(why); }
    s.parse().map_err(|_| why)
}

fn fixed_hex<const N: usize>(s: &str) -> Option<[u8; N]> {
    // tls::from_hex also accepts colon separators. The file format has exactly two digits per byte.
    if s.len() != N * 2 { return None; }
    from_hex(s)
}

fn decode_name(s: &str) -> Result<String, &'static str> {
    if s == "-" { return Ok(String::new()); }
    if s.len() % 2 != 0 || !s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err("expected lower-case hex for device name");
    }
    let bytes = (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect();
    String::from_utf8(bytes).map_err(|_| "device name is not UTF-8")
}

/// Takes the fields this version knows out of a device's extensions. A known field that doesn't
/// parse is an error, like any other bad record.
fn take_path(extra: &mut Vec<String>) -> Result<(PathMemory, bool), &'static str> {
    let (mut path, mut paused) = (PathMemory::default(), false);
    let mut rest = Vec::new();
    for field in extra.drain(..) {
        let (key, value) = field.split_once('=').expect("extensions hold one '='");
        match key {
            "addr" => path.addr = Some(value.parse().map_err(|_| "invalid remembered address")?),
            "lan" => path.lan = Some(value.parse().map_err(|_| "invalid remembered LAN address")?),
            "paused" if value == "1" => paused = true,
            "paused" => return Err("invalid paused value"),
            _ => rest.push(field),
        }
    }
    *extra = rest;
    Ok((path, paused))
}

fn extensions(fields: &[&str]) -> Result<Vec<String>, &'static str> {
    fields.iter().map(|&field| {
        match field.split_once('=') {
            Some((key, value)) if !key.is_empty() && !value.contains('=') => Ok(field.to_owned()),
            _ => Err("expected trailing key=value with a nonempty key and exactly one '='"),
        }
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_home() -> Home {
        let mut nonce = [0; 16];
        makepad_network::tls::os_random(&mut nonce).unwrap();
        Home::open(&std::env::temp_dir().join(format!("makepad-create-peers-{}", crate::hex(&nonce)))).unwrap()
    }

    fn remove_home(home: Home) {
        let dir = home.dir.clone();
        drop(home);
        fs::remove_dir_all(dir).unwrap();
    }

    fn key(byte: u8) -> PublicKey {
        let mut key = [byte; 65];
        key[0] = 4;
        key
    }

    fn store_text(path: &Path) -> String {
        String::from_utf8(unprotect_at_rest(PURPOSE, &fs::read(path).unwrap()).unwrap()).unwrap()
    }

    #[test]
    fn a_damaged_store_fails_closed() {
        let home = test_home();
        let path = home.peers_path();
        let bad = |text: &[u8], line: usize, problem: &str| {
            let (stored, _) = protect_at_rest(PURPOSE, text).unwrap();
            write_private(&path, &stored).unwrap();
            let error = Peers::load(&path).expect_err("a malformed store fails the whole load");
            assert_eq!(error.kind(), ErrorKind::InvalidData, "{problem} is invalid data");
            let message = error.to_string();
            assert!(message.contains(&format!("line {line}:")) && message.contains(problem),
                "the error should locate {problem} on line {line}, got {message}");
            assert_eq!(fs::read(&path).unwrap(), stored, "a failed load leaves {problem} untouched");
        };
        let id = crate::hex(&[0xab; 32]);
        let public = crate::hex(&key(0xcd));
        let town = format!("v1 town {id} 12 active");
        let device = format!("v1 device {public} words 10 20 7 active unchecked 61");
        bad(b"\n", 1, "v1 peers");
        bad(b"v2 peers\n", 1, "v1 peers");
        bad(b"v1 peers extra\n", 1, "v1 peers");
        bad(b"v1 peers", 1, "newline");
        bad(b"v1 peers\n\xff\n", 2, "UTF-8");
        for (record, problem) in [
            (town.replace("v1", "v2"), "version"),
            (town.replace("town", "member"), "record kind"),
            (String::new(), "empty field"),
            (town.replace("town ", "town  "), "empty field"),
            (format!(" {device}"), "empty field"),
            (format!("{town} "), "empty field"),
            (device.replace(" words ", "\twords "), "tab"),
            (format!("{town} future=a\tb"), "tab"),
            (format!("v1 town {id} 12"), "fields"),
            (device.replace(" unchecked 61", " unchecked"), "fields"),
            (town.replace(&id, &id[2..]), "town id"),
            (town.replace(&id, &format!("z{}", &id[1..])), "hex"),
            (device.replace(&public, &public[2..]), "device key"),
            (device.replace(&public, &format!("{public}00")), "device key"),
            (device.replace(&public, &format!("04:{}", &public[2..])), "hex"),
            (device.replace(&public, &format!("0g{}", &public[2..])), "hex"),
            (device.replace(&public, &format!("02{}", &public[2..])), "04"),
            (town.replace("12", "-1"), "founding time"),
            (town.replace("active", "removed"), "town state"),
            (device.replace("words", "local"), "how"),
            (device.replace(" 10 ", " none "), "first link time"),
            (device.replace(" 20 ", " 18446744073709551616 "), "last link time"),
            (device.replace(" 20 ", " 9 "), "predates"),
            (device.replace(" 7 ", " 65536 "), "version"),
            (device.replace("active", "left"), "device state"),
            (device.replace("unchecked", "yes"), "checked"),
            (device.replace(" 61", " 6"), "hex"),
            (device.replace(" 61", " x1"), "hex"),
            (device.replace(" 61", " 6A"), "lower-case"),
            (device.replace(" 61", " ff"), "UTF-8"),
            (format!("{town} =value"), "key=value"),
            (format!("{device} bare"), "key=value"),
            (format!("{device} a=b=c"), "key=value"),
        ] {
            bad(format!("{HEADER}{record}\n").as_bytes(), 2, problem);
        }
        bad(format!("{HEADER}{town}\n{device}\n{town}\n").as_bytes(), 4, "duplicate town");
        bad(format!("{HEADER}{device}\n{town}\n{device}\n").as_bytes(), 4, "duplicate device");
        let complete = format!("{HEADER}{town}\n{device}\n");
        bad(&complete.as_bytes()[..complete.len() - 1], 3, "newline");
        bad(&complete.as_bytes()[..complete.len() - 12], 3, "newline");
        #[cfg(windows)]
        {
            fs::write(&path, &complete).unwrap();
            let error = Peers::load(&path).expect_err("Windows requires a protected store");
            assert_eq!(error.kind(), ErrorKind::InvalidData, "unprotected Windows bytes are invalid data");
            assert_eq!(fs::read(&path).unwrap(), complete.as_bytes(), "unprotect failure leaves the file untouched");
        }
        remove_home(home);
    }

    #[test]
    fn the_store_round_trips() {
        let home = test_home();
        let path = home.peers_path();
        let (a, b, c) = (key(1), key(2), key(3));
        let id = [0xcc; 32];
        let mut peers = Peers::create(&path).unwrap();
        assert_eq!(store_text(&path), "v1 peers\n", "creation writes just the header");
        peers.linked(a, How::Words, 3, "Café 北", 10).unwrap();
        peers.add_town(id, 11).unwrap();
        peers.linked(b, How::Words, 2, "", 12).unwrap();
        peers.linked(c, How::Town, 1, "C", 13).unwrap();
        let loaded = Peers::load(&path).unwrap();
        assert_eq!(loaded.devices(), peers.devices(), "all device fields reload in order");
        assert_eq!(loaded.towns(), peers.towns(), "all town fields reload in order");
        let text = store_text(&path);
        let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
        assert!(lines[1].ends_with("words 10 10 3 active unchecked 436166c3a920e58c97"),
            "the first contact stores its times, version, state and lower-case UTF-8 name");
        assert!(lines[2].starts_with("v1 town ") && lines[2].ends_with(" 11 active"), "the town keeps its position and founding time");
        assert!(lines[3].ends_with("unchecked -"), "a missing name is a single dash");
        assert!(lines[4].ends_with("town 13 13 1 active unchecked 43"), "a device learned from a town keeps its origin");
        lines[1].push_str(" z=One empty= z=Two");
        lines[2].push_str(" next=β x=01");
        let (stored, _) = protect_at_rest(PURPOSE, (lines.join("\n") + "\n").as_bytes()).unwrap();
        write_private(&path, &stored).unwrap();
        assert_eq!(Peers::create(&path).unwrap_err().kind(), ErrorKind::AlreadyExists, "create refuses an existing store");
        assert_eq!(fs::read(&path).unwrap(), stored, "refused creation keeps every stored byte");

        let mut peers = Peers::load(&path).unwrap();
        peers.set_checked(&a, true).unwrap();
        peers.set_state(&a, DeviceState::Removed).unwrap();
        peers.set_state(&b, DeviceState::Blocked).unwrap();
        peers.set_town_state(&id, TownState::Left).unwrap();
        peers.linked(a, How::Town, 1, "Renamed", 30).unwrap();
        peers.linked(c, How::Words, 4, "C", 31).unwrap();
        let d = peers.device(&a).expect("a removed device keeps its key");
        assert_eq!((d.how, d.first, d.last, d.link_version), (How::Words, 10, 30, 3), "relink keeps origin, first time and highest version");
        assert_eq!((&*d.name, d.state, d.checked), ("Café 北", DeviceState::Removed, true), "relink keeps the original name and local marks");
        for (now, version) in [(20, 4), (9, 5)] {
            peers.linked(a, How::Town, version, "", now).unwrap();
            let d = peers.device(&a).unwrap();
            assert_eq!((d.first, d.last, d.link_version), (10, 30, version), "clock rollback preserves both times and records the link version");
            assert_eq!(d.name, "Café 北", "an empty reconnect name preserves the known label");
        }
        assert_eq!(peers.pins(), [Pin { device: c, min_version: 4 }], "only active keys become pins, at their highest version");
        let reloaded = Peers::load(&path).unwrap();
        assert_eq!(reloaded.devices(), peers.devices(), "relink and local marks survive restart");
        assert_eq!(reloaded.towns()[0].state, TownState::Left, "a town can be marked left");
        let text = store_text(&path);
        let lines: Vec<_> = text.lines().collect();
        assert!(lines[1].ends_with(" z=One empty= z=Two"), "ordered opaque device fields survive relink");
        assert!(lines[2].ends_with(" left next=β x=01"), "ordered opaque town fields survive a state change");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600, "the store is private");
        }
        assert_eq!(peers.set_state(&key(9), DeviceState::Removed).unwrap_err().kind(), ErrorKind::NotFound, "an unknown device has no state to change");
        assert_eq!(peers.set_checked(&key(9), true).unwrap_err().kind(), ErrorKind::NotFound, "an unknown device cannot be checked");
        assert_eq!(peers.add_town(id, 100).unwrap_err().kind(), ErrorKind::AlreadyExists, "a town cannot be added twice");
        assert_eq!(peers.set_town_state(&[0; 32], TownState::Left).unwrap_err().kind(), ErrorKind::NotFound, "an unknown town has no state to change");
        assert!(peers.linked([0; 65], How::Words, 1, "", 0).is_err(), "a malformed key cannot enter the store");

        let before = fs::read(&path).unwrap();
        fs::create_dir(path.with_extension("new")).unwrap(); // Prevent write_private from staging the next save.
        assert!(peers.set_state(&b, DeviceState::Active).is_err(), "a failed unblock is reported");
        assert!(peers.add_town([0; 32], 32).is_err(), "a failed addition is reported");
        assert_eq!(peers.devices(), reloaded.devices(), "failed changes leave device memory intact");
        assert_eq!(peers.towns(), reloaded.towns(), "failed additions leave town memory intact");
        assert_eq!(fs::read(&path).unwrap(), before, "failed saves leave the original bytes intact");
        remove_home(home);
    }

    #[test]
    fn the_home_record_decides_a_missing_store() {
        let home = test_home();
        let path = home.peers_path();
        let record = home.record_path();
        assert!(!path.exists() && !record.exists(), "opening the home itself leaves both files absent");
        assert_eq!(Peers::load(&path).unwrap_err().kind(), ErrorKind::NotFound, "a bare load passes through a missing file");
        Peers::open(&home).unwrap();
        assert_eq!(store_text(&path), HEADER, "a new home gets an empty store");
        assert_eq!(fs::read_to_string(&record).unwrap(), "v1 home\npeers\n", "a new home records the store");
        fs::remove_file(&path).unwrap();
        let error = Peers::open(&home).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::NotFound, "a listed store must be present");
        assert_eq!(error.to_string(), "local/peers is missing", "the error names the missing file");
        assert!(!path.exists(), "a listed missing store stays missing");
        assert_eq!(fs::read_to_string(&record).unwrap(), "v1 home\npeers\n", "a missing listed store leaves the record intact");

        fs::remove_file(&record).unwrap();
        fs::write(home.log_path(), []).unwrap();
        // A failed second write leaves a complete store for migration to pick up on retry.
        fs::create_dir(record.with_extension("new")).unwrap();
        assert!(Peers::open(&home).is_err(), "failure to write the home record is reported");
        assert!(!record.exists(), "failed migration leaves the record absent");
        assert_eq!(store_text(&path), HEADER, "the complete store is published before the record");
        fs::remove_dir(record.with_extension("new")).unwrap();
        let mut peers = Peers::open(&home).unwrap();
        assert_eq!(store_text(&path), HEADER, "an older home with a log gains an empty store");
        assert_eq!(fs::read_to_string(&record).unwrap(), "v1 home\npeers\n", "the migrated home records peers");
        peers.linked(key(7), How::Words, 1, "known", 10).unwrap();
        let stored = fs::read(&path).unwrap();
        fs::remove_file(&record).unwrap();
        let reopened = Peers::open(&home).unwrap();
        assert_eq!(reopened.devices(), peers.devices(), "an unlisted store recovers its linked devices");
        assert_eq!(fs::read(&path).unwrap(), stored, "record recovery leaves the store untouched");
        assert_eq!(fs::read_to_string(&record).unwrap(), "v1 home\npeers\n", "the missing record is restored");

        fs::write(&record, "v1 home\nother store  \n\nfuture").unwrap();
        Peers::open(&home).unwrap();
        assert_eq!(fs::read_to_string(&record).unwrap(), "v1 home\nother store  \n\nfuture\npeers\n",
            "adding peers preserves unknown lines, spaces and order");
        fs::remove_file(&path).unwrap();
        fs::write(&record, "v2 home\nfuture\n").unwrap();
        assert_eq!(Peers::open(&home).unwrap_err().kind(), ErrorKind::InvalidData, "a bad home header fails closed");
        assert!(!path.exists(), "a bad home header cannot create a store");
        assert_eq!(fs::read_to_string(&record).unwrap(), "v2 home\nfuture\n", "a bad home header is left for recovery");
        fs::write(&record, "v1 home\nfuture\n").unwrap();
        let (damaged, _) = protect_at_rest(PURPOSE, b"v1 peers\nv2 town").unwrap();
        write_private(&path, &damaged).unwrap();
        assert_eq!(Peers::open(&home).unwrap_err().kind(), ErrorKind::InvalidData, "migration refuses an unreadable store");
        assert_eq!(fs::read(&path).unwrap(), damaged, "migration preserves the damaged store");
        assert_eq!(fs::read_to_string(&record).unwrap(), "v1 home\nfuture\n", "migration only records a usable store");

        fs::remove_file(&record).unwrap();
        fs::create_dir(&record).unwrap();
        let error = fs::read(&record).expect_err("a directory cannot be read as a record file");
        assert_eq!(Peers::forget(&home, 45).unwrap_err().kind(), error.kind(), "forget propagates a filesystem read error");
        assert!(record.is_dir(), "a record with a filesystem error is left in place");
        assert_eq!(fs::read(&path).unwrap(), damaged, "a record read error stops the reset before the store moves");
        remove_home(home);
    }

    #[test]
    fn forgetting_preserves_the_old_stores_and_records_each_reset() {
        let home = test_home();
        let path = home.peers_path();
        let record = home.record_path();
        let first_aside = path.with_file_name("peers.unreadable-42");
        let second_aside = path.with_file_name("peers.unreadable-42-2");
        let damaged = b"damaged bytes\x00\xff";
        fs::write(&path, damaged).unwrap();
        fs::write(&record, "v1 home\nfuture line").unwrap();
        let mut peers = Peers::forget(&home, 42).unwrap();
        assert_eq!(fs::read(&first_aside).unwrap(), damaged, "forget preserves the damaged bytes exactly");
        assert_eq!(store_text(&path), HEADER, "forget creates a protected header-only store");
        assert!(peers.devices().is_empty() && peers.towns().is_empty(), "forget returns an empty store");
        assert!(Peers::open(&home).unwrap().pins().is_empty(), "the replacement loads through the home record");
        let first_record = "v1 home\nfuture line\npeers\nforgot peers 42 peers.unreadable-42\n";
        assert_eq!(fs::read_to_string(&record).unwrap(), first_record, "forget preserves other lines and appends the registration and aside name");

        peers.linked(key(8), How::Words, 1, "known", 42).unwrap();
        let second = fs::read(&path).unwrap();
        Peers::forget(&home, 42).unwrap();
        assert_eq!(fs::read(&first_aside).unwrap(), damaged, "another forget in the same second preserves the first aside");
        assert_eq!(fs::read(&second_aside).unwrap(), second, "the second store is preserved under the -2 name");
        let second_record = format!("{first_record}forgot peers 42 peers.unreadable-42-2\n");
        assert_eq!(fs::read_to_string(&record).unwrap(), second_record, "the second forget records its own aside and keeps one peers line");

        fs::remove_file(&path).unwrap();
        Peers::forget(&home, 43).unwrap();
        assert_eq!(store_text(&path), HEADER, "forget recreates a missing listed store");
        assert!(!path.with_file_name("peers.unreadable-43").exists(), "forgetting a missing file creates no aside");
        let third_record = format!("{second_record}forgot peers 43 -\n");
        assert_eq!(fs::read_to_string(&record).unwrap(), third_record, "a dash records that there was no store to move");

        let bad_header = b"v2 home\npeers\n";
        for (bytes, peers_name, home_name) in [
            (&bad_header[..], "peers.unreadable-42-3", "home.unreadable-42"),
            (&b"v1 home\npeers\n\xff\n"[..], "peers.unreadable-42-4", "home.unreadable-42-2"),
        ] {
            fs::write(&record, bytes).unwrap();
            let old_store = fs::read(&path).unwrap();
            assert_eq!(Peers::open(&home).unwrap_err().kind(), ErrorKind::InvalidData, "a malformed record disables linking until recovery");
            Peers::forget(&home, 42).unwrap();
            assert_eq!(fs::read(record.with_file_name(home_name)).unwrap(), bytes, "the malformed record is preserved byte for byte");
            assert_eq!(fs::read(path.with_file_name(peers_name)).unwrap(), old_store, "the store is preserved under its own available name");
            assert_eq!(fs::read_to_string(&record).unwrap(), format!("v1 home\npeers\nforgot peers 42 {peers_name}\nforgot home 42 {home_name}\n"),
                "the fresh record registers peers and records both filenames in order");
            let reopened = Peers::open(&home).unwrap();
            assert!(reopened.devices().is_empty() && reopened.towns().is_empty(), "the fresh record opens the empty store");
        }
        assert_eq!(fs::read(record.with_file_name("home.unreadable-42")).unwrap(), bad_header, "a same-second reset preserves the first record aside");

        let before = fs::read(&path).unwrap();
        let before_record = fs::read(&record).unwrap();
        fs::create_dir(path.with_extension("new")).unwrap();
        assert!(Peers::forget(&home, 44).is_err(), "forget reports a failed replacement write");
        assert_eq!(fs::read(path.with_file_name("peers.unreadable-44")).unwrap(), before, "a failed replacement keeps the moved store");
        assert!(!path.exists(), "a failed replacement never publishes a partial store");
        assert_eq!(fs::read(&record).unwrap(), before_record, "forget stops before recording a failed replacement");
        remove_home(home);
    }
    #[test]
    fn a_devices_remembered_path_and_pause_round_trip_and_old_files_still_load() {
        let home = test_home();
        let path = home.peers_path();
        let mut peers = Peers::create(&path).unwrap();
        let (a, b, c) = (key(1), key(2), key(3));
        for (k, name, at) in [(a, "Ada", 10), (b, "Bob", 20), (c, "Cy", 30)] {
            peers.linked(k, How::Words, 3, name, at).unwrap();
        }
        assert_eq!(peers.carrier_pin(c).unwrap().1, PathMemory::default());
        let seen = PathMemory { addr: Some("192.0.2.7:4000".parse().unwrap()), lan: None };
        for k in [a, b, c] {
            peers.remember_path(&k, seen).unwrap();
        }
        peers.remember_path(&b, PathMemory { lan: Some("192.0.2.12:6000".parse().unwrap()), ..PathMemory::default() }).unwrap();
        assert_eq!(peers.device(&b).unwrap().path, PathMemory { lan: Some("192.0.2.12:6000".parse().unwrap()), ..seen },
            "an address that wasn't seen this time keeps its old value");
        assert!([a, b, c].into_iter().all(|k| peers.carrier_pin(k).is_some()));
        peers.set_paused(&c, true).unwrap();
        assert!(peers.carrier_pin(c).is_none());
        peers.set_state(&b, DeviceState::Blocked).unwrap();
        assert!(peers.carrier_pin(b).is_none());
        peers.set_state(&a, DeviceState::Removed).unwrap();
        assert_eq!(peers.carrier_pin(a), None, "nor a removed one");
        assert!(peers.device(&c).unwrap().paused, "the pause is on the device");
        let reloaded = Peers::load(&path).unwrap();
        assert_eq!(reloaded.devices(), peers.devices(), "the path and the pause survive a restart");
        peers.set_paused(&c, false).unwrap();
        assert_eq!(Peers::load(&path).unwrap().carrier_pin(c).map(|(p, _)| p.device), Some(c), "connecting again lifts the pause");

        // A file from before these fields loads with none of them, and keeps fields it doesn't know.
        let public = crate::hex(&key(9));
        let old = format!("v1 peers\nv1 device {public} words 10 20 7 active unchecked 61 port=70000 future=1\n");
        let (stored, _) = protect_at_rest(PURPOSE, old.as_bytes()).unwrap();
        write_private(&path, &stored).unwrap();
        let mut peers = Peers::load(&path).unwrap();
        let device = peers.device(&key(9)).unwrap();
        assert_eq!((device.path, device.paused), (PathMemory::default(), false), "an old record has no path and no pause");
        peers.set_paused(&key(9), true).unwrap();
        assert!(store_text(&path).ends_with(" paused=1 port=70000 future=1\n"), "an unknown field is kept after the known ones");
        for bad in ["addr=nonsense", "lan=192.0.2.12", "paused=yes"] {
            let text = format!("v1 peers\nv1 device {public} words 10 20 7 active unchecked 61 {bad}\n");
            let (stored, _) = protect_at_rest(PURPOSE, text.as_bytes()).unwrap();
            write_private(&path, &stored).unwrap();
            assert!(Peers::load(&path).is_err(), "{bad} fails closed");
        }
        remove_home(home);
    }
}

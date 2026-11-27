//! The key proof: who is at the other end of a link. TLS authenticates nobody here. This runs
//! inside a TLS session of the strict profile (a fresh certificate per connection, no resumption,
//! no renegotiation), after the handshake and before any other byte. It is sans-IO: plaintext
//! bytes in, plaintext bytes out.
//!
//! It binds itself to that TLS session through the server certificate's hash, which both sides
//! put in the transcript `T0`. A relay that terminates TLS toward each side holds two
//! certificates, so the two sides see two different `T0`s, and no MAC or signature carries from
//! one to the other.
//!
//! ```text
//! S -> C  Hello  { versions_s, nonce_s }
//! C -> S  Gate   { version, nonce_c, mac_c }
//! S -> C  Proof  { mac_s, auth_pub_s, sig_s }       after mac_c verifies
//! C -> S  Proof  { auth_pub_c, sig_c }              after mac_s, sig_s and the key check
//! first contact, server first, each after the peer's Proof (and, for the client, its Intro):
//!         Intro  { device_pub, sig_invite, sig_device }
//! each side, once its last check has passed:
//!         Keys   { contribution, name }
//! either side, on a version mismatch before completion:
//!         Versions { count, accepted versions }
//! ```
//!
//! - **Two devices, no towns.** A link belongs to a pair of devices and carries every town the two
//!   share; nothing about towns crosses here. A first contact pins two devices and founds
//!   nothing. Events are bound to their town by their own signatures.
//! - **The gate.** Each side proves it holds the gate key before the other shows any key. On
//!   first contact the gate key comes from the two invites read aloud. On reconnect it comes from
//!   the link secret of the two device keys. A stranger, or a relay that terminates TLS, gets two
//!   nonces and a MAC it can neither check nor link to anyone.
//! - **The server isn't told who calls.** Its caller gives it the devices that may be on this
//!   path (normally one), and the client's gate picks among them.
//! - **The key check.** On first contact the peer's auth key is its invite key, checked against
//!   the commitment in its words, after the signature. On reconnect it is the pinned device key,
//!   compared byte for byte.
//! - A version mismatch discloses only the versions we accept. Other refusals stay local.
//! - **Application bytes.** Until completion the proof accepts its next message or a version
//!   notice. Bytes after the peer's last Keys are handed back as application bytes.

use crate::key::{hkdf_sha256, verify, DeviceKey, Domain, PublicKey};
use makepad_network::digest::sha256_hash;
use makepad_network::p2p::words::{Invite, COMMIT_BITS};
use makepad_network::tls::{constant_time_eq, ecdsa_raw_from_der, x509::ecdsa_sig_from_raw};
use makepad_network::tunnel::hmac_sha256;
use std::sync::Arc;

/// The link protocol's version.
pub const VERSION: u16 = 15;
// Recognised so an older peer gets a version notice. Only VERSION can complete the proof.
const RECOGNISED: std::ops::Range<u16> = 1..VERSION;

/// The labels of the link proof and of the link around it ([`crate::connect`]), in the same form
/// as [`crate::key::label`] and kept apart from those by the same test.
pub(crate) mod label {
    pub const LINK_GATE: &[u8] = b"makepad create v1 link gate\0";
    pub const FIRST_GATE: &[u8] = b"makepad create v1 first gate\0";
    pub const GATE_KEY: &[u8] = b"makepad create v1 gate key\0";
    pub const LINK_KEYS: &[u8] = b"makepad create v1 link keys\0";
    /// The punch tag of a first contact, from both invites.
    pub const FIRST_TAG: &[u8] = b"makepad create v1 first tag\0";
    /// The stream's connection id, from both punch nonces.
    pub const STREAM_ID: &[u8] = b"makepad create v1 stream id\0";
    /// The key of a reconnect's probe tags, from the link secret.
    pub const PROBE_KEY: &[u8] = b"makepad create v1 probe key\0";
    /// The hour's reconnect tag, under that key.
    pub const PROBE_TAG: &[u8] = b"makepad create v1 probe tag\0";

    #[cfg(test)]
    pub const ALL: &[&[u8]] = &[LINK_GATE, FIRST_GATE, GATE_KEY, LINK_KEYS, FIRST_TAG, STREAM_ID, PROBE_KEY, PROBE_TAG];
}

// Message types: `type u8 | len u16 | body`.
const HELLO: u8 = 1;
const GATE: u8 = 2;
const PROOF: u8 = 3;
const INTRO: u8 = 4;
const KEYS: u8 = 5;
const VERSIONS: u8 = 6;

// Transcript modes.
const FIRST_CONTACT: u8 = 1;
const RECONNECT: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Server,
    Client,
}

impl Role {
    fn byte(self) -> u8 {
        match self {
            Role::Server => 1,
            Role::Client => 2,
        }
    }
}

/// This side of a first contact by words.
pub struct FirstContact {
    /// The one-hour key committed to in our words. It signs our Proof and our Intro.
    pub invite_key: Arc<DeviceKey>,
    pub own_invite: Invite,
    /// The peer's invite as typed from its words.
    pub peer_invite: Invite,
    pub device: Arc<DeviceKey>,
    /// Our display name. It crosses only after the last check.
    pub name: String,
}

/// A device this side has pinned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pin {
    pub device: PublicKey,
    /// The highest link version this pair has used. A lower one is refused.
    pub min_version: u16,
}

/// This side of a reconnect between two devices that pinned each other.
pub struct Reconnect {
    pub device: Arc<DeviceKey>,
    /// Who may be at the other end. A client names the one device it dials. A server names the
    /// devices whose probe tag punched this path (normally one), all above its own id, and the
    /// client's gate picks one. Who may be a candidate is the caller's rule: contacts, its own
    /// devices and members of its closed towns, never the members of a wider town.
    pub peers: Vec<Pin>,
}

pub enum Mode {
    FirstContact(FirstContact),
    Reconnect(Reconnect),
}

/// Why a proof failed. Only a version mismatch has a wire notice.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A message that breaks the format, comes out of order or repeats, or an application byte
    /// before the proof ends.
    Format(&'static str),
    /// The devices need a common Town version at least as new as this pair has used.
    /// A peer's Versions notice reports the same refusal.
    Version { peer_newest: Option<u16> },
    /// The gate MAC didn't verify: not a device we pinned, not the invite as read, or someone is
    /// between the two sides. On reconnect a device with a new key ends here too, and, with more
    /// than two people, one that hasn't yet learnt of us: the caller words it by its history.
    Gate,
    Signature,
    /// The peer's invite key doesn't match the commitment in its words.
    Commitment,
    /// A reconnect proof by another key than the pinned one, after the gate passed. Only a peer
    /// that holds the pinned key gets that far, so this is a broken peer; a new key fails the gate.
    KeyChanged,
    /// Both sides are the same: a connection to ourselves, or our own words read back.
    SelfLink,
    /// The caller's candidates fit no role: none, more than one for a client, or some on each
    /// side of our own id (the lower id serves).
    Candidates,
    /// Signing or key agreement failed in the OS.
    Crypto(String),
}

/// One direction's keys, derived from both sides' contributions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkKeys {
    pub stream_send: [u8; 32],
    pub stream_recv: [u8; 32],
    pub datagram_send: [u8; 32],
    pub datagram_recv: [u8; 32],
}

/// A completed proof.
#[derive(Debug)]
pub struct Linked {
    pub peer_device: PublicKey,
    pub peer_name: String,
    pub version: u16,
    pub keys: LinkKeys,
    /// What followed the peer's last proof message: the first application bytes.
    pub leftover: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    AwaitHello,
    AwaitGate,
    AwaitProof,
    AwaitIntro,
    AwaitKeys,
    Done,
}

pub struct Proof {
    mode: Mode,
    role: Role,
    cert_hash: [u8; 32],
    nonce: [u8; 32],
    contribution: [u8; 32],
    own_id: Vec<u8>,
    /// The peer's id; a server of a reconnect learns it from the client's gate.
    peer_id: Vec<u8>,
    /// The candidate the gate picked (always 0 but for a reconnect server).
    peer: usize,
    state: State,
    t0: Vec<u8>,
    k_gate: [u8; 32],
    auth_s: Option<PublicKey>,
    auth_c: Option<PublicKey>,
    peer_device: Option<PublicKey>,
    /// The first version is accepted. The server refuses a client that selects any of the rest.
    offered: Vec<u16>,
    version: u16,
    input: Vec<u8>,
    output: Vec<u8>,
    failed: Option<Refusal>,
}

/// The first-contact id: kind, hint, public IPv4 and port, then the commitment. Kind 0x03
/// appends the LAN IPv4 and port; kind 0x01 is the short form.
pub(crate) fn invite_id(invite: &Invite) -> Vec<u8> {
    let mut id = vec![if invite.lan.is_some() { 0x03 } else { 0x01 }, invite.nat as u8];
    id.extend_from_slice(&invite.addr.ip().octets());
    id.extend_from_slice(&invite.addr.port().to_be_bytes());
    id.extend_from_slice(&(invite.commit & ((1 << COMMIT_BITS) - 1)).to_be_bytes());
    if let Some(lan) = invite.lan {
        id.extend_from_slice(&lan.ip().octets());
        id.extend_from_slice(&lan.port().to_be_bytes());
    }
    id
}

/// Our id, the candidates' ids, and our role (the lower id serves).
fn ids_and_role(mode: &Mode) -> Result<(Vec<u8>, Vec<Vec<u8>>, Role), Refusal> {
    let (own_id, peer_ids): (Vec<u8>, Vec<Vec<u8>>) = match mode {
        Mode::FirstContact(f) => (invite_id(&f.own_invite), vec![invite_id(&f.peer_invite)]),
        Mode::Reconnect(r) => (device_id(&r.device.public()), r.peers.iter().map(|pin| device_id(&pin.device)).collect()),
    };
    if peer_ids.contains(&own_id) {
        return Err(Refusal::SelfLink);
    }
    let role = match &peer_ids[..] {
        [] => return Err(Refusal::Candidates),
        ids if ids.iter().all(|id| own_id < *id) => Role::Server,
        [_] => Role::Client,
        _ => return Err(Refusal::Candidates),
    };
    Ok((own_id, peer_ids, role))
}

/// The role `mode` takes, for opening the transport below the proof in it.
pub fn role(mode: &Mode) -> Result<Role, Refusal> {
    Ok(ids_and_role(mode)?.2)
}

/// The reconnect id of a device: `0x02 | device_pub`.
fn device_id(device: &PublicKey) -> Vec<u8> {
    [&[0x02][..], device].concat()
}

fn push16(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
    out.extend_from_slice(bytes);
}

/// `T0 = mode u8 | version u16 | len, versions_s | cert_hash | nonce_s | nonce_c | len, id_lower |
/// len, id_higher`: every field has a fixed length or a two-byte length before it.
#[allow(clippy::too_many_arguments)]
fn transcript(mode: u8, version: u16, versions_s: &[u16], cert_hash: &[u8; 32], nonce_s: &[u8; 32], nonce_c: &[u8; 32], a: &[u8], b: &[u8]) -> Vec<u8> {
    let (lower, higher) = if a < b { (a, b) } else { (b, a) };
    let mut t = vec![mode];
    t.extend_from_slice(&version.to_be_bytes());
    let versions: Vec<u8> = versions_s.iter().flat_map(|v| v.to_be_bytes()).collect();
    push16(&mut t, &versions);
    t.extend_from_slice(cert_hash);
    t.extend_from_slice(nonce_s);
    t.extend_from_slice(nonce_c);
    push16(&mut t, lower);
    push16(&mut t, higher);
    t
}

/// Strict DER: the encoding a P-256 signature has exactly one of.
fn strict_der(sig: &[u8]) -> bool {
    ecdsa_raw_from_der(sig).is_some_and(|raw| ecdsa_sig_from_raw(&raw) == sig)
}

fn gate_mac(k_gate: &[u8; 32], role: Role, t0: &[u8]) -> [u8; 32] {
    hmac_sha256(k_gate, &[label::LINK_GATE, &[role.byte()], t0].concat())
}

fn frame(out: &mut Vec<u8>, kind: u8, body: &[u8]) {
    out.push(kind);
    push16(out, body);
}

fn push_sig(out: &mut Vec<u8>, sig: &[u8]) {
    out.push(sig.len() as u8);
    out.extend_from_slice(sig);
}

struct Reader<'b>(&'b [u8]);

impl<'b> Reader<'b> {
    fn take(&mut self, n: usize) -> Result<&'b [u8], Refusal> {
        if self.0.len() < n {
            return Err(Refusal::Format("short message"));
        }
        let (head, rest) = self.0.split_at(n);
        self.0 = rest;
        Ok(head)
    }

    fn u8(&mut self) -> Result<u8, Refusal> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Refusal> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn bytes32(&mut self) -> Result<[u8; 32], Refusal> {
        Ok(self.take(32)?.try_into().unwrap())
    }

    fn point(&mut self) -> Result<PublicKey, Refusal> {
        let point: PublicKey = self.take(65)?.try_into().unwrap();
        if point[0] != 0x04 {
            return Err(Refusal::Format("not an uncompressed point"));
        }
        Ok(point)
    }

    fn sig(&mut self) -> Result<&'b [u8], Refusal> {
        let len = self.u8()? as usize;
        let sig = self.take(len)?;
        if !strict_der(sig) {
            return Err(Refusal::Format("not a strict DER signature"));
        }
        Ok(sig)
    }

    fn end(self) -> Result<(), Refusal> {
        if self.0.is_empty() { Ok(()) } else { Err(Refusal::Format("trailing bytes")) }
    }
}

fn crypto(e: std::io::Error) -> Refusal {
    Refusal::Crypto(e.to_string())
}

impl Proof {
    /// A proof for `mode` in the TLS session whose server certificate hashes to `cert_hash`.
    /// `random` is 64 bytes from the OS: our nonce, then our key contribution. The role follows
    /// from the mode (the lower id serves); the caller opens TLS in that role.
    pub fn new(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, std::iter::once(VERSION).chain(RECOGNISED).collect())
    }

    #[cfg(test)]
    /// A v1 offer and selection policy with this build's notice handling.
    pub(crate) fn version_one(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, vec![1])
    }

    #[cfg(test)]
    fn version_two(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, vec![2, 1])
    }

    #[cfg(test)]
    pub(crate) fn version_three(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, vec![3, 1, 2])
    }

    #[cfg(test)]
    fn version_four(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, vec![4, 1, 2, 3])
    }

    #[cfg(test)]
    fn version_five(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, vec![5, 1, 2, 3, 4])
    }

    #[cfg(test)]
    fn version_six(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, vec![6, 1, 2, 3, 4, 5])
    }

    #[cfg(test)]
    fn version_seven(mode: Mode, cert_hash: [u8; 32], random: [u8; 64]) -> Result<Proof, Refusal> {
        Self::start(mode, cert_hash, random, vec![7, 1, 2, 3, 4, 5, 6])
    }

    fn start(mode: Mode, cert_hash: [u8; 32], random: [u8; 64], offered: Vec<u16>) -> Result<Proof, Refusal> {
        let (own_id, peer_ids, role) = ids_and_role(&mode)?;
        // A server of a reconnect learns its peer's id from the gate.
        let peer_id = if peer_ids.len() == 1 { peer_ids[0].clone() } else { Vec::new() };
        let mut proof = Proof {
            mode,
            role,
            cert_hash,
            nonce: random[..32].try_into().unwrap(),
            contribution: random[32..].try_into().unwrap(),
            own_id,
            peer_id,
            peer: 0,
            state: if role == Role::Server { State::AwaitGate } else { State::AwaitHello },
            t0: Vec::new(),
            k_gate: [0; 32],
            auth_s: None,
            auth_c: None,
            peer_device: None,
            offered,
            version: 0,
            input: Vec::new(),
            output: Vec::new(),
            failed: None,
        };
        if role == Role::Server {
            let mut body = vec![proof.offered.len() as u8];
            for version in &proof.offered {
                body.extend_from_slice(&version.to_be_bytes());
            }
            body.extend_from_slice(&proof.nonce);
            frame(&mut proof.output, HELLO, &body);
        }
        Ok(proof)
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// The bytes to write into TLS, in order and before any application byte.
    pub fn take_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.output)
    }

    /// Takes TLS plaintext. `Ok(Some)` once the proof completed; after a refusal every call
    /// returns it again.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<Option<Linked>, Refusal> {
        if let Some(refusal) = &self.failed {
            return Err(refusal.clone());
        }
        if self.state == State::Done {
            return Err(Refusal::Format("the proof is already complete"));
        }
        self.input.extend_from_slice(bytes);
        while self.input.len() >= 3 {
            let len = u16::from_be_bytes([self.input[1], self.input[2]]) as usize;
            if self.input.len() < 3 + len {
                break;
            }
            let kind = self.input[0];
            let body: Vec<u8> = self.input[3..3 + len].to_vec();
            self.input.drain(..3 + len);
            let result = self.on_message(kind, &body).map_err(|refusal| self.refuse(refusal, kind));
            if let Some(mut linked) = result? {
                linked.leftover = std::mem::take(&mut self.input);
                return Ok(Some(linked));
            }
        }
        // Check even a partial frame's kind, allowing a notice at every proof stage.
        if let Some(&kind) = self.input.first() {
            if kind != self.expected() && kind != VERSIONS {
                return Err(self.refuse(Refusal::Format("unexpected message"), kind));
            }
        }
        Ok(None)
    }

    fn refuse(&mut self, reason: Refusal, received: u8) -> Refusal {
        self.output.clear();
        if matches!(reason, Refusal::Version { .. }) && received != VERSIONS {
            let [hi, lo] = self.offered[0].to_be_bytes();
            frame(&mut self.output, VERSIONS, &[1, hi, lo]);
        }
        self.failed = Some(reason.clone());
        reason
    }

    fn expected(&self) -> u8 {
        match self.state {
            State::AwaitHello => HELLO,
            State::AwaitGate => GATE,
            State::AwaitProof => PROOF,
            State::AwaitIntro => INTRO,
            State::AwaitKeys | State::Done => KEYS,
        }
    }

    fn first_contact(&self) -> bool {
        matches!(self.mode, Mode::FirstContact(_))
    }

    fn auth_key(&self) -> &DeviceKey {
        match &self.mode {
            Mode::FirstContact(f) => &f.invite_key,
            Mode::Reconnect(r) => &r.device,
        }
    }

    fn device(&self) -> &DeviceKey {
        match &self.mode {
            Mode::FirstContact(f) => &f.device,
            Mode::Reconnect(r) => &r.device,
        }
    }

    fn on_message(&mut self, kind: u8, body: &[u8]) -> Result<Option<Linked>, Refusal> {
        let mut r = Reader(body);
        if kind == VERSIONS {
            let count = r.u8()? as usize;
            let versions = r.take(2 * count)?;
            r.end()?;
            let peer_newest = versions.as_chunks::<2>().0.iter().copied().map(u16::from_be_bytes).max();
            return Err(Refusal::Version { peer_newest });
        }
        if kind != self.expected() {
            return Err(Refusal::Format("unexpected message"));
        }
        match self.state {
            State::AwaitHello => {
                let count = r.u8()? as usize;
                let mut versions = Vec::with_capacity(count);
                for _ in 0..count {
                    versions.push(r.u16()?);
                }
                let nonce_s = r.bytes32()?;
                r.end()?;
                let min = self.pin().map_or(0, |pin| pin.min_version);
                let version = self.offered[0];
                if version < min || !versions.contains(&version) {
                    return Err(Refusal::Version { peer_newest: versions.iter().copied().max() });
                }
                let (t0, k_gate) = self.keyed(0, version, &versions, nonce_s, self.nonce)?;
                (self.version, self.t0, self.k_gate) = (version, t0, k_gate);
                let mut gate = version.to_be_bytes().to_vec();
                gate.extend_from_slice(&self.nonce);
                gate.extend_from_slice(&self.mac(Role::Client));
                frame(&mut self.output, GATE, &gate);
                self.state = State::AwaitProof;
            }
            State::AwaitGate => {
                let version = r.u16()?;
                let nonce_c = r.bytes32()?;
                let mac_c = r.bytes32()?;
                r.end()?;
                if !self.offered.contains(&version) {
                    return Err(Refusal::Format("version not offered"));
                }
                if version != self.offered[0] {
                    return Err(Refusal::Version { peer_newest: Some(version) });
                }
                self.pick(version, nonce_c, &mac_c)?;
                if version < self.pin().map_or(0, |pin| pin.min_version) {
                    return Err(Refusal::Version { peer_newest: Some(version) });
                }
                let auth = self.auth_key().public();
                self.auth_s = Some(auth);
                let sig = self.auth_key().sign(Domain::Link, &[&[0x01][..], &self.t0, &auth].concat()).map_err(crypto)?;
                let mut proof = self.mac(Role::Server).to_vec();
                proof.extend_from_slice(&auth);
                push_sig(&mut proof, &sig);
                frame(&mut self.output, PROOF, &proof);
                self.state = State::AwaitProof;
            }
            State::AwaitProof if self.role == Role::Client => {
                let mac_s = r.bytes32()?;
                let auth_s = r.point()?;
                let sig_s = r.sig()?;
                r.end()?;
                if !constant_time_eq(&mac_s, &self.mac(Role::Server)) {
                    return Err(Refusal::Gate);
                }
                if !verify(Domain::Link, &auth_s, &[&[0x01][..], &self.t0, &auth_s].concat(), sig_s) {
                    return Err(Refusal::Signature);
                }
                self.check_key(&auth_s)?;
                self.auth_s = Some(auth_s);
                let auth_c = self.auth_key().public();
                self.auth_c = Some(auth_c);
                let sig = self.auth_key().sign(Domain::Link, &[&[0x02][..], &self.t0, &auth_s, &auth_c].concat()).map_err(crypto)?;
                let mut proof = auth_c.to_vec();
                push_sig(&mut proof, &sig);
                frame(&mut self.output, PROOF, &proof);
                if self.first_contact() {
                    self.state = State::AwaitIntro;
                } else {
                    self.peer_device = Some(auth_s);
                    self.send_keys();
                    self.state = State::AwaitKeys;
                }
            }
            State::AwaitProof => {
                let auth_c = r.point()?;
                let sig_c = r.sig()?;
                r.end()?;
                let auth_s = self.auth_s.unwrap();
                if !verify(Domain::Link, &auth_c, &[&[0x02][..], &self.t0, &auth_s, &auth_c].concat(), sig_c) {
                    return Err(Refusal::Signature);
                }
                self.check_key(&auth_c)?;
                self.auth_c = Some(auth_c);
                if self.first_contact() {
                    self.send_intro()?;
                    self.state = State::AwaitIntro;
                } else {
                    self.peer_device = Some(auth_c);
                    self.send_keys();
                    self.state = State::AwaitKeys;
                }
            }
            State::AwaitIntro => {
                let device = r.point()?;
                let sig_invite = r.sig()?;
                let sig_device = r.sig()?;
                r.end()?;
                let peer_role = if self.role == Role::Server { Role::Client } else { Role::Server };
                let peer_auth = if self.role == Role::Server { self.auth_c } else { self.auth_s }.unwrap();
                let msg = self.intro_message(peer_role, &device);
                if !verify(Domain::DeviceIntro, &peer_auth, &msg, sig_invite) || !verify(Domain::DeviceIntro, &device, &msg, sig_device) {
                    return Err(Refusal::Signature);
                }
                if device == self.device().public() {
                    return Err(Refusal::SelfLink);
                }
                self.peer_device = Some(device);
                if self.role == Role::Client {
                    self.send_intro()?;
                }
                self.send_keys();
                self.state = State::AwaitKeys;
            }
            State::AwaitKeys => {
                let contribution = r.bytes32()?;
                let len = r.u16()? as usize;
                let name = String::from_utf8(r.take(len)?.to_vec()).map_err(|_| Refusal::Format("name is not UTF-8"))?;
                r.end()?;
                self.state = State::Done;
                return Ok(Some(self.linked(contribution, name)));
            }
            State::Done => unreachable!(),
        }
        Ok(None)
    }

    /// `T0` and the gate key for candidate `peer`, once the version and both nonces are known.
    fn keyed(&self, peer: usize, version: u16, versions_s: &[u16], nonce_s: [u8; 32], nonce_c: [u8; 32]) -> Result<(Vec<u8>, [u8; 32]), Refusal> {
        let (mode, peer_id) = match &self.mode {
            Mode::FirstContact(_) => (FIRST_CONTACT, self.peer_id.clone()),
            Mode::Reconnect(r) => (RECONNECT, device_id(&r.peers[peer].device)),
        };
        let t0 = transcript(mode, version, versions_s, &self.cert_hash, &nonce_s, &nonce_c, &self.own_id, &peer_id);
        let k_gate = match &self.mode {
            Mode::FirstContact(_) => {
                let (lower, higher) = if self.own_id < peer_id { (&self.own_id, &peer_id) } else { (&peer_id, &self.own_id) };
                sha256_hash(&[label::FIRST_GATE, lower, higher].concat())
            }
            Mode::Reconnect(r) => hmac_sha256(&r.device.link_secret(&r.peers[peer].device).map_err(crypto)?, label::GATE_KEY),
        };
        Ok((t0, k_gate))
    }

    /// The server's side of the gate: the candidate whose gate key makes the client's MAC. Every
    /// candidate is tried, so the time doesn't tell which one matched.
    fn pick(&mut self, version: u16, nonce_c: [u8; 32], mac_c: &[u8; 32]) -> Result<(), Refusal> {
        let count = match &self.mode { Mode::Reconnect(r) => r.peers.len(), Mode::FirstContact(_) => 1 };
        let mut picked = None;
        for peer in 0..count {
            let (t0, k_gate) = self.keyed(peer, version, &self.offered, self.nonce, nonce_c)?;
            if constant_time_eq(mac_c, &gate_mac(&k_gate, Role::Client, &t0)) && picked.is_none() {
                picked = Some((peer, t0, k_gate));
            }
        }
        let (peer, t0, k_gate) = picked.ok_or(Refusal::Gate)?;
        if let Mode::Reconnect(r) = &self.mode {
            self.peer_id = device_id(&r.peers[peer].device);
        }
        (self.peer, self.version, self.t0, self.k_gate) = (peer, version, t0, k_gate);
        Ok(())
    }

    /// The pinned peer of a reconnect, once known.
    fn pin(&self) -> Option<&Pin> {
        match &self.mode { Mode::Reconnect(r) => r.peers.get(self.peer), Mode::FirstContact(_) => None }
    }

    fn mac(&self, role: Role) -> [u8; 32] {
        gate_mac(&self.k_gate, role, &self.t0)
    }

    /// First contact: the peer's invite key against the commitment in its words (after its
    /// signature verified). Reconnect: the peer's key against the pin.
    fn check_key(&self, auth: &PublicKey) -> Result<(), Refusal> {
        match &self.mode {
            Mode::FirstContact(f) => if commitment_matches(&f.peer_invite, auth) { Ok(()) } else { Err(Refusal::Commitment) },
            Mode::Reconnect(r) => if constant_time_eq(auth, &r.peers[self.peer].device) { Ok(()) } else { Err(Refusal::KeyChanged) },
        }
    }

    fn intro_message(&self, role: Role, device: &PublicKey) -> Vec<u8> {
        let mut msg = vec![role.byte()];
        msg.extend_from_slice(&self.t0);
        msg.extend_from_slice(&self.auth_s.unwrap());
        msg.extend_from_slice(&self.auth_c.unwrap());
        msg.extend_from_slice(device);
        msg
    }

    fn send_intro(&mut self) -> Result<(), Refusal> {
        let Mode::FirstContact(f) = &self.mode else { unreachable!() };
        let device = f.device.public();
        let msg = self.intro_message(self.role, &device);
        let sig_invite = f.invite_key.sign(Domain::DeviceIntro, &msg).map_err(crypto)?;
        let sig_device = f.device.sign(Domain::DeviceIntro, &msg).map_err(crypto)?;
        let mut intro = device.to_vec();
        push_sig(&mut intro, &sig_invite);
        push_sig(&mut intro, &sig_device);
        frame(&mut self.output, INTRO, &intro);
        Ok(())
    }

    fn send_keys(&mut self) {
        let name = match &self.mode { Mode::FirstContact(f) => f.name.as_str(), Mode::Reconnect(_) => "" };
        let mut keys = self.contribution.to_vec();
        push16(&mut keys, name.as_bytes());
        frame(&mut self.output, KEYS, &keys);
    }

    fn linked(&self, peer_contribution: [u8; 32], peer_name: String) -> Linked {
        let (contribution_s, contribution_c) = if self.role == Role::Server { (self.contribution, peer_contribution) } else { (peer_contribution, self.contribution) };
        let salt = sha256_hash(&[&self.t0[..], &self.auth_s.unwrap(), &self.auth_c.unwrap()].concat());
        let mut okm = [0; 128];
        hkdf_sha256(&salt, &[contribution_s, contribution_c].concat(), label::LINK_KEYS, &mut okm);
        let block = |i: usize| -> [u8; 32] { okm[i * 32..(i + 1) * 32].try_into().unwrap() };
        // Server to client first, for the stream and then for datagrams.
        let (send, recv) = if self.role == Role::Server { (0, 1) } else { (1, 0) };
        Linked {
            peer_device: self.peer_device.unwrap(),
            peer_name,
            version: self.version,
            keys: LinkKeys { stream_send: block(send), stream_recv: block(recv), datagram_send: block(2 + send), datagram_recv: block(2 + recv) },
            leftover: Vec::new(),
        }
    }
}

/// How long an invite's words can be used: the commitment is safe against grinding for this long.
pub const INVITE_LIFETIME_MS: u64 = 60 * 60 * 1000;

/// The invite of one Connect screen. Its key lives in memory only and serves at most one pairing:
/// it is gone after the hour, once a pairing claims it, or when the screen closes.
pub struct InviteSession {
    key: Option<std::sync::Arc<DeviceKey>>,
    opened_ms: u64,
}

impl InviteSession {
    pub fn open(now_ms: u64) -> std::io::Result<InviteSession> {
        Ok(InviteSession { key: Some(std::sync::Arc::new(DeviceKey::generate()?)), opened_ms: now_ms })
    }

    /// The key for a new connection on this invite, while it still serves.
    pub fn key(&mut self, now_ms: u64) -> Option<std::sync::Arc<DeviceKey>> {
        if now_ms.saturating_sub(self.opened_ms) >= INVITE_LIFETIME_MS {
            self.key = None;
        }
        self.key.clone()
    }

    /// A first contact on this invite completed. The first claim wins and ends the invite; a
    /// later one (another connection that was already running) gets `false` and must not pair.
    pub fn claim(&mut self, now_ms: u64) -> bool {
        let live = self.key(now_ms).is_some();
        self.key = None;
        live
    }

    /// The Connect screen closed.
    pub fn close(&mut self) {
        self.key = None;
    }
}

/// `Invite::matches`, the 2^20 stretch. Test builds remember answers (see [`remember`]) so many
/// scenarios can share two invites; an answer is never invented, only recalled.
fn commitment_matches(invite: &Invite, key: &PublicKey) -> bool {
    #[cfg(test)]
    {
        let id = (format!("{invite:?}"), *key);
        if let Some(&seen) = SEEN.lock().unwrap().get_or_insert_with(Default::default).get(&id) {
            return seen;
        }
        let answer = invite.matches(key);
        remember(invite, key, answer);
        answer
    }
    #[cfg(not(test))]
    invite.matches(key)
}

#[cfg(test)]
#[allow(clippy::type_complexity)]
static SEEN: std::sync::Mutex<Option<std::collections::HashMap<(String, PublicKey), bool>>> = std::sync::Mutex::new(None);

/// Records what `Invite::matches` answers for `key`, from a commitment computed for that key.
#[cfg(test)]
pub(crate) fn remember(invite: &Invite, key: &PublicKey, answer: bool) {
    SEEN.lock().unwrap().get_or_insert_with(Default::default).insert((format!("{invite:?}"), *key), answer);
}

#[cfg(test)]
mod tests;

use super::*;
use crate::key::DeviceKey;
use makepad_network::p2p::discover::Hint;
use std::sync::{Arc, OnceLock};

/// One person's side of a first contact: the invite they read out, its key, and their device.
struct Side {
    invite_key: Arc<DeviceKey>,
    invite: Invite,
    device: Arc<DeviceKey>,
}

struct Fixture {
    a: Side,
    b: Side,
    /// A third device: another contact, a changed key or an impostor.
    c: Arc<DeviceKey>,
}

/// Two invites, each with its commitment computed once (the 2^20 stretch) and remembered.
fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let side = |addr: &str| {
            let invite_key = Arc::new(DeviceKey::generate().unwrap());
            let addr = addr.parse().unwrap();
            let invite = Invite { nat: Hint::Easy, addr, lan: None, commit: Invite::commitment(Hint::Easy, addr, None, &invite_key.public()) };
            remember(&invite, &invite_key.public(), true);
            Side { invite_key, invite, device: Arc::new(DeviceKey::generate().unwrap()) }
        };
        // A's address sorts below B's, so A serves a first contact between them.
        Fixture { a: side("192.0.2.1:4000"), b: side("198.51.100.2:5000"), c: Arc::new(DeviceKey::generate().unwrap()) }
    })
}

fn random(seed: u8) -> [u8; 64] {
    std::array::from_fn(|i| seed.wrapping_mul(31).wrapping_add(i as u8))
}

fn first(me: &Side, peer: &Invite, name: &str) -> Mode {
    Mode::FirstContact(FirstContact {
        invite_key: me.invite_key.clone(), own_invite: me.invite.clone(), peer_invite: peer.clone(), device: me.device.clone(), name: name.into(),
    })
}

fn pins(peers: &[&DeviceKey], min_version: u16) -> Vec<Pin> {
    peers.iter().map(|peer| Pin { device: peer.public(), min_version }).collect()
}

fn again(device: &Arc<DeviceKey>, peer: &DeviceKey, min_version: u16) -> Mode {
    Mode::Reconnect(Reconnect { device: device.clone(), peers: pins(&[peer], min_version) })
}

#[derive(Debug)]
enum Outcome {
    Linked(Linked),
    Refused(Refusal),
    Pending,
}

impl Outcome {
    fn linked(&self) -> &Linked {
        match self {
            Outcome::Linked(linked) => linked,
            other => panic!("expected a link, got {other:?}"),
        }
    }
}

struct Run {
    a: Outcome,
    b: Outcome,
    a_sent: Vec<u8>,
    b_sent: Vec<u8>,
}

fn feed(proof: &mut Proof, outcome: &mut Outcome, bytes: &[u8]) {
    if bytes.is_empty() || !matches!(outcome, Outcome::Pending) {
        return;
    }
    match proof.feed(bytes) {
        Ok(Some(linked)) => *outcome = Outcome::Linked(linked),
        Ok(None) => {}
        Err(refusal) => *outcome = Outcome::Refused(refusal),
    }
}

/// Shuttles bytes between two proofs as TLS would, until neither has anything to say.
fn run(mut a: Proof, mut b: Proof) -> Run {
    let mut r = Run { a: Outcome::Pending, b: Outcome::Pending, a_sent: Vec::new(), b_sent: Vec::new() };
    loop {
        let (to_b, to_a) = (a.take_output(), b.take_output());
        if to_b.is_empty() && to_a.is_empty() {
            return r;
        }
        r.a_sent.extend_from_slice(&to_b);
        r.b_sent.extend_from_slice(&to_a);
        feed(&mut b, &mut r.b, &to_b);
        feed(&mut a, &mut r.a, &to_a);
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// The frames in a run of proof bytes, as (type, body).
fn frames(mut bytes: &[u8]) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    while bytes.len() >= 3 {
        let len = u16::from_be_bytes([bytes[1], bytes[2]]) as usize;
        out.push((bytes[0], bytes[3..3 + len].to_vec()));
        bytes = &bytes[3 + len..];
    }
    out
}

fn framed(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    frame(&mut out, kind, body);
    out
}

#[test]
fn labels_are_prefix_free_across_the_key_and_the_link() {
    let all: Vec<&[u8]> = crate::key::label::ALL.iter().chain(label::ALL).copied().collect();
    for (i, a) in all.iter().enumerate() {
        assert!(a.ends_with(b"\0") && a.iter().filter(|&&b| b == 0).count() == 1, "label {i} ends in its only NUL");
        for b in &all[i + 1..] {
            assert!(!a.starts_with(b) && !b.starts_with(a), "labels are prefix-free");
        }
    }
}

/// Walks one frame by its layout and returns the points in it, or `None` when the body is not
/// exactly those fields.
fn walk(kind: u8, body: &[u8], server: bool) -> Option<Vec<PublicKey>> {
    let mut r = Reader(body);
    let mut points = Vec::new();
    match (kind, server) {
        (HELLO, true) => {
            let count = r.u8().ok()? as usize;
            r.take(2 * count + 32).ok()?;
        }
        (GATE, false) => {
            r.take(2 + 32 + 32).ok()?;
        }
        (PROOF, _) => {
            if server {
                r.take(32).ok()?;
            }
            points.push(r.point().ok()?);
            r.sig().ok()?;
        }
        (INTRO, _) => {
            points.push(r.point().ok()?);
            r.sig().ok()?;
            r.sig().ok()?;
        }
        (KEYS, _) => {
            r.take(32).ok()?;
            let len = r.u16().ok()? as usize;
            r.take(len).ok()?;
        }
        _ => return None,
    }
    r.end().ok()?;
    Some(points)
}

#[test]
fn devices_pin_each_other_and_relink() {
    let f = fixture();
    // First contact: each side learns the other's device and name, and nothing else is made.
    let run1 = run(
        Proof::new(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1)).unwrap(),
        Proof::new(first(&f.b, &f.a.invite, "Bob"), [1; 32], random(2)).unwrap(),
    );
    let (a, b) = (run1.a.linked(), run1.b.linked());
    assert_eq!((a.peer_device, b.peer_device), (f.b.device.public(), f.a.device.public()), "each side learns the other's device key");
    assert_eq!((a.peer_name.as_str(), b.peer_name.as_str()), ("Bob", "Ada"), "names cross after the last check");
    assert_eq!((a.keys.stream_send, a.keys.datagram_send), (b.keys.stream_recv, b.keys.datagram_recv), "A's send keys are B's receive keys");
    assert_eq!((a.keys.stream_recv, a.keys.datagram_recv), (b.keys.stream_send, b.keys.datagram_send), "and the other way");
    let all = [a.keys.stream_send, a.keys.stream_recv, a.keys.datagram_send, a.keys.datagram_recv];
    assert!((0..4).all(|i| (i + 1..4).all(|j| all[i] != all[j])), "four distinct keys");
    assert!(a.leftover.is_empty(), "no application bytes after the proof");
    assert_eq!((a.version, b.version), (VERSION, VERSION), "both devices linked at the current version");
    let first_stream = a.keys.stream_send;

    // Reconnects: each pair of three devices proves itself with no words, one link per pair.
    let devices = [&f.a.device, &f.b.device, &f.c];
    let mut runs = vec![("first contact", run1, vec![f.a.invite_key.public(), f.b.invite_key.public(), f.a.device.public(), f.b.device.public()])];
    for (i, j) in [(0, 1), (0, 2), (1, 2)] {
        let r = run(
            Proof::new(again(devices[i], devices[j], 1), [3; 32], random(5)).unwrap(),
            Proof::new(again(devices[j], devices[i], 1), [3; 32], random(6)).unwrap(),
        );
        assert_eq!(r.a.linked().peer_device, devices[j].public(), "link {i}-{j}: the pinned key proved itself");
        assert_eq!(r.a.linked().keys.stream_send, r.b.linked().keys.stream_recv, "link {i}-{j}: shared keys");
        assert_ne!(r.a.linked().keys.stream_send, first_stream, "link {i}-{j}: keys belong to their session");
        runs.push(("reconnect", r, vec![devices[i].public(), devices[j].public()]));
    }

    // No town id crosses in the proof: every frame of both modes is exactly its listed fields,
    // and the only keys in them are the two sides' own.
    for (mode, r, keys) in &runs {
        let a_serves = r.a_sent.first() == Some(&HELLO);
        for (sent, server) in [(&r.a_sent, a_serves), (&r.b_sent, !a_serves)] {
            for (kind, body) in frames(sent) {
                let points = walk(kind, &body, server).unwrap_or_else(|| panic!("{mode}: frame {kind} is exactly its fields"));
                assert!(points.iter().all(|p| keys.contains(p)), "{mode}: frame {kind} carries only the two sides' keys");
            }
        }
    }
}

#[test]
fn a_server_picks_its_caller_from_the_candidates() {
    let f = fixture();
    let mut keys = [&f.a.device, &f.b.device, &f.c];
    keys.sort_by_key(|k| k.public());
    let [server, mid, high] = keys;
    // The server is told both may call. Each is picked by its own gate, not by its place in the list.
    for client in [mid, high] {
        let r = run(
            Proof::new(Mode::Reconnect(Reconnect { device: server.clone(), peers: pins(&[mid, high], 1) }), [1; 32], random(1)).unwrap(),
            Proof::new(again(client, server, 1), [1; 32], random(2)).unwrap(),
        );
        assert_eq!(r.a.linked().peer_device, client.public(), "the server links the device that called");
        assert_eq!(r.b.linked().peer_device, server.public(), "and the caller its server");
    }
    // A caller the server wasn't told of fails the gate, and the server shows no key.
    let r = run(
        Proof::new(Mode::Reconnect(Reconnect { device: server.clone(), peers: pins(&[mid], 1) }), [1; 32], random(1)).unwrap(),
        Proof::new(again(high, server, 1), [1; 32], random(2)).unwrap(),
    );
    assert!(matches!(r.a, Outcome::Refused(Refusal::Gate)), "an unknown caller: {:?}", r.a);
    assert!(!contains(&r.a_sent, &server.public()), "the server's key never crosses");
    // Candidates that fit no role are the caller's mistake.
    let cases: [(&str, &Arc<DeviceKey>, &[&DeviceKey]); 3] = [
        ("no candidate", server, &[]),
        ("a client with two peers", high, &[server, mid]),
        ("candidates on both sides", mid, &[server, high]),
    ];
    for (what, device, peers) in cases {
        let proof = Proof::new(Mode::Reconnect(Reconnect { device: device.clone(), peers: pins(peers, 1) }), [1; 32], random(1));
        assert_eq!(proof.err(), Some(Refusal::Candidates), "{what}");
    }
}

#[test]
fn a_mitm_relay_is_refused() {
    // The relay terminates TLS toward both sides: two sessions, two server certificates. It
    // forwards every proof byte unchanged.
    let f = fixture();
    let devices = [f.a.device.public(), f.b.device.public(), f.a.invite_key.public(), f.b.invite_key.public()];
    let runs = [
        run(Proof::new(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1)).unwrap(),
            Proof::new(first(&f.b, &f.a.invite, "Bob"), [7; 32], random(2)).unwrap()),
        run(Proof::new(again(&f.a.device, &f.b.device, 1), [1; 32], random(1)).unwrap(),
            Proof::new(again(&f.b.device, &f.a.device, 1), [7; 32], random(2)).unwrap()),
    ];
    for (mode, r) in ["first contact", "reconnect"].iter().zip(runs) {
        let (server, client) = if matches!(r.a, Outcome::Refused(_)) { (&r.a, &r.b) } else { (&r.b, &r.a) };
        assert!(matches!(server, Outcome::Refused(Refusal::Gate)), "{mode}: the server refuses the client's gate: {server:?}");
        assert!(matches!(client, Outcome::Pending), "{mode}: the client never gets a proof: {client:?}");
        for sent in [&r.a_sent, &r.b_sent] {
            assert!(frames(sent).len() == 1, "{mode}: only the Hello and the Gate cross");
            for key in &devices {
                assert!(!contains(sent, key), "{mode}: no key crosses to the relay");
            }
        }
    }
}

#[test]
fn a_changed_peer_key_is_refused() {
    // A pins B's old device key; B comes back with a new one. The gate needs both pinned keys, so
    // it fails before either side shows a key. Pick the old key so both still agree who serves.
    let f = fixture();
    let (a, b) = (&f.a.device, &f.b.device);
    let old = std::iter::repeat_with(|| DeviceKey::generate().unwrap())
        .find(|old| (a.public() < old.public()) == (a.public() < b.public()))
        .unwrap();
    let r = run(Proof::new(again(a, &old, 1), [1; 32], random(1)).unwrap(), Proof::new(again(b, a, 1), [1; 32], random(2)).unwrap());
    let refused = [&r.a, &r.b].into_iter().filter(|o| matches!(o, Outcome::Refused(Refusal::Gate))).count();
    assert_eq!(refused, 1, "the server refuses the gate: {:?} / {:?}", r.a, r.b);
    for key in [a.public(), b.public(), old.public()] {
        assert!(!contains(&r.a_sent, &key) && !contains(&r.b_sent, &key), "no device key crosses");
    }
}

#[test]
fn a_replayed_proof_is_refused() {
    let f = fixture();
    let (a, b) = (&f.a.device, &f.b.device);
    let (server_key, client_key) = if a.public() < b.public() { (a, b) } else { (b, a) };
    let recorded = run(Proof::new(again(server_key, client_key, 1), [1; 32], random(1)).unwrap(),
                       Proof::new(again(client_key, server_key, 1), [1; 32], random(2)).unwrap());
    recorded.a.linked();
    // The client's recorded bytes against a new session: new certificate, new server nonce.
    let mut server = Proof::new(again(server_key, client_key, 1), [2; 32], random(3)).unwrap();
    assert_eq!(frames(&server.take_output())[0].0, HELLO, "the new server says Hello");
    assert_eq!(server.feed(&recorded.b_sent).unwrap_err(), Refusal::Gate, "a recorded client gate fails in a new session");
    assert!(server.take_output().is_empty(), "the new server shows no proof");
    // The server's recorded bytes against a new client: its Hello passes, its Proof doesn't.
    let mut client = Proof::new(again(client_key, server_key, 1), [2; 32], random(4)).unwrap();
    assert_eq!(client.feed(&recorded.a_sent).unwrap_err(), Refusal::Gate, "a recorded server proof fails for a new client nonce");
    assert!(client.take_output().is_empty(), "nothing queued after a refusal is sent");
}

/// A server and a client of a first contact between A and B, stepped to where the server has
/// just sent its Proof. Returns them with that Proof's bytes.
fn first_contact_at_server_proof() -> (Proof, Proof, Vec<u8>) {
    let f = fixture();
    let mut server = Proof::new(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1)).unwrap();
    let mut client = Proof::new(first(&f.b, &f.a.invite, "Bob"), [1; 32], random(2)).unwrap();
    assert_eq!((server.role(), client.role()), (Role::Server, Role::Client), "A's address sorts first");
    client.feed(&server.take_output()).unwrap();
    server.feed(&client.take_output()).unwrap();
    let proof = server.take_output();
    (server, client, proof)
}

#[test]
fn signatures_do_not_cross_purposes() {
    let f = fixture();
    let (server, _, proof) = first_contact_at_server_proof();
    let (_, body) = &frames(&proof)[0];
    let (t0, auth) = (server.t0.clone(), server.auth_s.unwrap());
    let server_msg = [&[0x01][..], &t0, &auth].concat();
    let cases: [(&str, Vec<u8>); 4] = [
        ("an Event signature is no Link proof", f.a.invite_key.sign(Domain::Event, &server_msg).unwrap()),
        ("an Intro signature is no Proof", f.a.invite_key.sign(Domain::DeviceIntro, &server_msg).unwrap()),
        ("a client proof is no server proof", f.a.invite_key.sign(Domain::Link, &[&[0x02][..], &t0, &auth].concat()).unwrap()),
        ("another key's proof is no proof", f.c.sign(Domain::Link, &server_msg).unwrap()),
    ];
    for (what, sig) in cases {
        let (_, mut client, _) = first_contact_at_server_proof();
        let mut forged = body[..32 + 65].to_vec();
        push_sig(&mut forged, &sig);
        assert_eq!(client.feed(&framed(PROOF, &forged)).unwrap_err(), Refusal::Signature, "{what}");
        assert!(client.take_output().is_empty(), "{what}: the client shows nothing after it");
    }
    // A connection to ourselves: our own words read back, or our own key pinned.
    let own = Proof::new(first(&f.a, &f.a.invite, "Ada"), [1; 32], random(1));
    assert_eq!(own.err(), Some(Refusal::SelfLink), "our own words read back");
    let own = Proof::new(again(&f.a.device, &f.a.device, 1), [1; 32], random(1));
    assert_eq!(own.err(), Some(Refusal::SelfLink), "our own key as the peer");
}

#[test]
fn the_transcript_encoding_is_injective() {
    let t = transcript(FIRST_CONTACT, 2, &[2, 1], &[0xaa; 32], &[0xbb; 32], &[0xcc; 32], b"\x01c", b"\x01ab");
    let expected = [
        &[1, 0, 2, 0, 4, 0, 2, 0, 1][..], &[0xaa; 32], &[0xbb; 32], &[0xcc; 32], &[0, 3, 1, b'a', b'b'], &[0, 2, 1, b'c'],
    ].concat();
    assert_eq!(t, expected, "known-answer bytes, ids in byte order");
    let other = transcript(FIRST_CONTACT, 2, &[2, 1], &[0xaa; 32], &[0xbb; 32], &[0xcc; 32], b"\x01a", b"\x01bc");
    assert_ne!(t, other, "moving a byte between the two ids changes the transcript");
    let reconnect = transcript(RECONNECT, 2, &[2, 1], &[0xaa; 32], &[0xbb; 32], &[0xcc; 32], b"\x01c", b"\x01ab");
    assert_ne!(t[0], reconnect[0], "the mode is the first byte");
    let shorter = transcript(FIRST_CONTACT, 2, &[2], &[0xaa; 32], &[0xbb; 32], &[0xcc; 32], b"\x01c", b"\x01ab");
    assert_eq!(&shorter[..7], &[1, 0, 2, 0, 2, 0, 2], "the offered versions carry their length");
}

/// A reconnect server stepped to where it waits for the client's Proof, with that honest Proof's
/// body.
fn reconnect_at_client_proof() -> (Proof, Vec<u8>) {
    let f = fixture();
    let (a, b) = (&f.a.device, &f.b.device);
    let (server_key, client_key) = if a.public() < b.public() { (a, b) } else { (b, a) };
    let mut server = Proof::new(again(server_key, client_key, 1), [1; 32], random(1)).unwrap();
    let mut client = Proof::new(again(client_key, server_key, 1), [1; 32], random(2)).unwrap();
    client.feed(&server.take_output()).unwrap();
    server.feed(&client.take_output()).unwrap();
    client.feed(&server.take_output()).unwrap();
    let (_, body) = frames(&client.take_output()).remove(0);
    (server, body)
}

#[test]
fn malformed_proof_messages_are_refused() {
    let (_, honest) = reconnect_at_client_proof();
    let (point, sig) = (&honest[..65], &honest[66..]);
    let ber = {
        // The same signature with a long-form length: BER, not DER.
        let mut ber = vec![0x30, 0x81];
        ber.extend_from_slice(&sig[1..]);
        ber
    };
    let cases: [(&str, Vec<u8>, Refusal); 5] = [
        ("a compressed point", [&[0x02][..], &point[1..], &[sig.len() as u8], sig].concat(), Refusal::Format("not an uncompressed point")),
        ("a point off the curve", [&[0x04][..], &[1; 64], &[sig.len() as u8], sig].concat(), Refusal::Signature),
        ("a BER signature", [point, &[ber.len() as u8], &ber].concat(), Refusal::Format("not a strict DER signature")),
        ("a trailing byte", [&honest[..], &[0]].concat(), Refusal::Format("trailing bytes")),
        ("a short message", honest[..70].to_vec(), Refusal::Format("short message")),
    ];
    for (what, body, refusal) in cases {
        let (mut server, _) = reconnect_at_client_proof();
        assert_eq!(server.feed(&framed(PROOF, &body)).unwrap_err(), refusal, "{what}");
    }
    let (mut server, _) = reconnect_at_client_proof();
    assert_eq!(server.feed(&framed(GATE, &[0; 66])).unwrap_err(), Refusal::Format("unexpected message"), "a repeated gate");
    let (mut server, _) = reconnect_at_client_proof();
    assert_eq!(server.feed(b"G").unwrap_err(), Refusal::Format("unexpected message"), "an application byte before the proof ends");
    let (mut server, _) = reconnect_at_client_proof();
    assert_eq!(server.feed(&[framed(PROOF, &honest), b"GET".to_vec()].concat()).unwrap_err(), Refusal::Format("unexpected message"),
        "application bytes coalesced behind the client's Proof");
    let f = fixture();
    let mut fresh = Proof::new(again(&f.a.device, &f.b.device, 1), [1; 32], random(1)).unwrap();
    let wrong = if fresh.role() == Role::Server { framed(PROOF, &honest) } else { framed(GATE, &[0; 66]) };
    assert_eq!(fresh.feed(&wrong).unwrap_err(), Refusal::Format("unexpected message"), "a message out of order");
    assert_eq!(fresh.feed(b"").unwrap_err(), Refusal::Format("unexpected message"), "a refusal sticks");
}

#[test]
fn first_contact_checks_the_words() {
    let f = fixture();
    // An impostor reads A's words but holds another invite key: B refuses at the commitment and
    // never shows its device key.
    let impostor = Side { invite_key: Arc::new(DeviceKey::generate().unwrap()), invite: f.a.invite.clone(), device: Arc::new(DeviceKey::generate().unwrap()) };
    let r = run(Proof::new(first(&impostor, &f.b.invite, "M"), [1; 32], random(1)).unwrap(),
                Proof::new(first(&f.b, &f.a.invite, "Bob"), [1; 32], random(2)).unwrap());
    assert!(matches!(r.b, Outcome::Refused(Refusal::Commitment)), "the impostor's key misses A's commitment: {:?}", r.b);
    assert!(!contains(&r.b_sent, &f.b.device.public()), "B's device key never reaches the impostor");

    // B heard A's port wrong: the ids differ, so the transcripts do, and the gate fails first.
    let misheard = Invite { addr: "192.0.2.1:4001".parse().unwrap(), ..f.a.invite.clone() };
    let r = run(Proof::new(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1)).unwrap(),
                Proof::new(first(&f.b, &misheard, "Bob"), [1; 32], random(2)).unwrap());
    assert!(matches!(r.a, Outcome::Refused(Refusal::Gate)), "a misheard address fails the gate: {:?}", r.a);

    // An Intro signed by the invite key but not by the device key it introduces.
    let (mut server, mut client, proof) = first_contact_at_server_proof();
    client.feed(&proof).unwrap();
    server.feed(&client.take_output()).unwrap();
    let (_, intro) = frames(&server.take_output()).remove(0);
    let msg = server.intro_message(Role::Server, &f.a.device.public());
    let mut forged = intro[..65].to_vec();
    push_sig(&mut forged, &f.a.invite_key.sign(Domain::DeviceIntro, &msg).unwrap());
    push_sig(&mut forged, &f.c.sign(Domain::DeviceIntro, &msg).unwrap());
    assert_eq!(client.feed(&framed(INTRO, &forged)).unwrap_err(), Refusal::Signature, "an Intro needs both signatures");
}

#[test]
fn link_versions_below_the_pair_are_refused() {
    let f = fixture();
    let (a, b) = (&f.a.device, &f.b.device);
    let (server_key, client_key) = if a.public() < b.public() { (a, b) } else { (b, a) };
    let r = run(Proof::new(again(server_key, client_key, 1), [1; 32], random(1)).unwrap(),
                Proof::new(again(client_key, server_key, VERSION + 1), [1; 32], random(2)).unwrap());
    assert!(matches!(r.b, Outcome::Refused(Refusal::Version { .. })), "a client whose pair used a newer version: {:?}", r.b);
    let r = run(Proof::new(again(server_key, client_key, VERSION + 1), [1; 32], random(1)).unwrap(),
                Proof::new(again(client_key, server_key, 1), [1; 32], random(2)).unwrap());
    assert!(matches!(r.a, Outcome::Refused(Refusal::Version { .. })), "a server whose pair used a newer version: {:?}", r.a);
    let mut client = Proof::new(again(client_key, server_key, 1), [1; 32], random(2)).unwrap();
    let hello = [&[1][..], &(VERSION + 1).to_be_bytes(), &[0; 32]].concat();
    assert_eq!(client.feed(&framed(HELLO, &hello)).unwrap_err(), Refusal::Version { peer_newest: Some(VERSION + 1) }, "an unknown version");
}

#[test]
fn incompatible_versions_notify_the_other_proof() {
    let f = fixture();
    type Constructor = fn(Mode, [u8; 32], [u8; 64]) -> Result<Proof, Refusal>;
    let mut hello_versions = vec![(1 + RECOGNISED.len()) as u8];
    hello_versions.extend(VERSION.to_be_bytes());
    for version in RECOGNISED { hello_versions.extend(version.to_be_bytes()); }
    let [hi, lo] = VERSION.to_be_bytes();
    for (index, old) in [Proof::version_one as Constructor, Proof::version_two, Proof::version_three, Proof::version_four,
                Proof::version_five, Proof::version_six, Proof::version_seven].into_iter().enumerate() {
        let previous = Refusal::Version { peer_newest: Some(index as u16 + 1) };
        for old_server in [true, false] {
            let server = if old_server { old } else { Proof::new };
            let client = if old_server { Proof::new } else { old };
            let mut server = server(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1)).unwrap();
            let mut client = client(first(&f.b, &f.a.invite, "Bob"), [1; 32], random(2)).unwrap();
            let hello = server.take_output();
            let (sender, receiver) = if old_server {
                assert_eq!(client.feed(&hello).unwrap_err(), previous, "our client refuses the older Hello");
                (&mut client, &mut server)
            } else {
                assert_eq!(&hello[3..3 + hello_versions.len()], hello_versions, "our Hello offers the current version, then the recognised versions");
                client.feed(&hello).unwrap();
                let mut gate = client.take_output();
                *gate.last_mut().unwrap() ^= 1;
                assert_eq!(server.feed(&gate).unwrap_err(), previous, "our server refuses the older version before checking its MAC");
                (&mut server, &mut client)
            };
            let notice = sender.take_output();
            assert_eq!(notice, framed(VERSIONS, &[1, hi, lo]), "only the accepted version crosses after refusal");
            receiver.feed(&notice[..1]).unwrap();
            assert_eq!(receiver.feed(&notice[1..]).unwrap_err(), Refusal::Version { peer_newest: Some(VERSION) }, "the other proof recognises a fragmented notice");
            assert!(receiver.take_output().is_empty(), "a received notice is never echoed");
        }
    }

    let mut server = Proof::new(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1)).unwrap();
    let mut gate = [0; 66];
    gate[..2].copy_from_slice(&(VERSION + 1).to_be_bytes());
    assert_eq!(server.feed(&framed(GATE, &gate)).unwrap_err(), Refusal::Format("version not offered"), "an unoffered Gate is malformed");
    assert!(server.take_output().is_empty(), "a format refusal discloses nothing");
}

#[test]
fn builds_that_share_no_version_both_end_with_a_version_refusal() {
    let f = fixture();
    let current: Vec<_> = std::iter::once(VERSION).chain(RECOGNISED).collect();
    for far_is_a in [true, false] {
        let (offered_a, offered_b) = if far_is_a { (vec![VERSION + 1], current.clone()) } else { (current.clone(), vec![VERSION + 1]) };
        let a = Proof::start(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1), offered_a).unwrap();
        let b = Proof::start(first(&f.b, &f.a.invite, "Bob"), [1; 32], random(2), offered_b).unwrap();
        let r = run(a, b);
        assert!(matches!(r.a, Outcome::Refused(Refusal::Version { .. })), "side a (far_is_a {far_is_a}): {:?}", r.a);
        assert!(matches!(r.b, Outcome::Refused(Refusal::Version { .. })), "side b (far_is_a {far_is_a}): {:?}", r.b);
    }

    // An older peer that predates the notice never sends one. What it sends instead is a message
    // of a kind this build doesn't know: a refusal, never a panic, and the proof stays refused.
    for body in [&[][..], &[1, 2, 3][..], &[0; 70][..]] {
        for role_server in [true, false] {
            let mut proof = Proof::new(first(&f.a, &f.b.invite, "Ada"), [1; 32], random(1)).unwrap();
            if (proof.role() == Role::Server) != role_server {
                proof = Proof::new(first(&f.b, &f.a.invite, "Bob"), [1; 32], random(2)).unwrap();
            }
            assert_eq!(proof.feed(&framed(9, body)).unwrap_err(), Refusal::Format("unexpected message"), "an unknown kind is refused");
            assert_eq!(proof.feed(b"").unwrap_err(), Refusal::Format("unexpected message"), "and the refusal sticks");
        }
    }
}

#[test]
fn the_invite_key_is_single_use() {
    let mut invite = InviteSession::open(1_000).unwrap();
    let key = invite.key(2_000).expect("a new invite serves");
    let again = invite.key(3_000).unwrap();
    assert_eq!(key.public(), again.public(), "one key for the invite's life");
    assert!(invite.claim(4_000), "the first completed pairing claims it");
    assert!(!invite.claim(5_000), "a second connection on the same invite gets no pairing");
    assert!(invite.key(6_000).is_none(), "no new connection after a pairing");

    let mut invite = InviteSession::open(1_000).unwrap();
    assert!(invite.key(1_000 + INVITE_LIFETIME_MS - 1).is_some(), "it serves for the hour");
    assert!(invite.key(1_000 + INVITE_LIFETIME_MS).is_none(), "and not after it");
    assert!(!invite.claim(1_000 + INVITE_LIFETIME_MS), "a pairing that ends after the hour is refused");

    let mut invite = InviteSession::open(1_000).unwrap();
    invite.close();
    assert!(invite.key(1_001).is_none() && !invite.claim(1_001), "closing the screen ends it");
}

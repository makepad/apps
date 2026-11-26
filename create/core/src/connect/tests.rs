use super::*;
use crate::key::DeviceKey;
use crate::link::{remember, FirstContact, Pin, Reconnect};
use makepad_network::p2p::discover::Hint;
use makepad_network::p2p::natsim::{LinkConfig, NatConfig, NatKind, Net};
use std::net::Ipv4Addr;
use std::sync::{Arc, OnceLock};

/// One person: their invite key, their device and the invite they read out.
struct Side {
    invite_key: Arc<DeviceKey>,
    device: Arc<DeviceKey>,
    invite: Invite,
}

/// Both invite forms for A, B behind another router, and B behind A's router.
struct Fixture {
    a: Side,
    b: Side,
    b_same: Side,
    a_lan: Side,
    b_lan: Side,
    b_same_lan: Side,
}

const A_PUBLIC: &str = "198.51.100.1:4000";
const B_PUBLIC: &str = "198.51.100.2:5000";
const B_SAME_PUBLIC: &str = "198.51.100.1:5000";
const A_LAN: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(10, 77, 0, 10), 4000);
const B_LAN: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(10, 77, 0, 20), 5000);

fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let side = |public: &str, lan: Option<SocketAddrV4>| {
            let invite_key = Arc::new(DeviceKey::generate().unwrap());
            let addr: SocketAddrV4 = public.parse().unwrap();
            let nat = Hint::ConeUnknownFiltering;
            let invite = Invite { nat, addr, lan, commit: Invite::commitment(nat, addr, lan, &invite_key.public()) };
            remember(&invite, &invite_key.public(), true);
            Side { invite_key, device: Arc::new(DeviceKey::generate().unwrap()), invite }
        };
        Fixture {
            a: side(A_PUBLIC, None), b: side(B_PUBLIC, None), b_same: side(B_SAME_PUBLIC, None),
            a_lan: side(A_PUBLIC, Some(A_LAN)), b_lan: side(B_PUBLIC, Some(B_LAN)), b_same_lan: side(B_SAME_PUBLIC, Some(B_LAN)),
        }
    })
}

fn mode(me: &Side, peer: &Side, name: &str) -> Mode {
    Mode::FirstContact(FirstContact {
        invite_key: me.invite_key.clone(), own_invite: me.invite.clone(), peer_invite: peer.invite.clone(), device: me.device.clone(), name: name.into(),
    })
}

fn random(seed: u8) -> [u8; 72] {
    std::array::from_fn(|i| seed.wrapping_mul(29).wrapping_add(i as u8))
}

#[derive(Clone, Copy)]
enum Routers {
    Separate,
    Shared { hairpin: bool },
}

/// Two connections on a simulated internet, each on its own socket behind its NAT.
struct World {
    net: Net,
    local: [SocketAddr; 2],
    conns: [Connection; 2],
    destinations: [Vec<SocketAddr>; 2],
}

impl World {
    fn new(routers: Routers, a: &Side, b: &Side, seed: u64) -> World {
        let mut net = Net::new(seed, LinkConfig { latency_ms: 20, jitter_ms: 5, loss_per_mille: 20 });
        let mut config = NatConfig::new(NatKind::PortRestricted);
        config.hairpin = matches!(routers, Routers::Shared { hairpin: true });
        let nat_a = net.add_nat(Ipv4Addr::new(198, 51, 100, 1), config);
        let nat_b = match routers {
            Routers::Shared { .. } => nat_a,
            Routers::Separate => net.add_nat(Ipv4Addr::new(198, 51, 100, 2), NatConfig::new(NatKind::PortRestricted)),
        };
        let local = [
            a.invite.lan.unwrap_or_else(|| "192.0.2.10:4000".parse().unwrap()),
            b.invite.lan.unwrap_or_else(|| "192.0.2.20:5000".parse().unwrap()),
        ];
        net.add_host(*local[0].ip(), Some(nat_a));
        net.add_host(*local[1].ip(), Some(nat_b));
        let (mode_a, mode_b) = (mode(a, b, "Ada"), mode(b, a, "Bob"));
        let tag = first_contact_tag(&mode_a).unwrap();
        assert_eq!(Some(tag), first_contact_tag(&mode_b), "both sides derive one punch tag");
        let conns = [
            Connection::new(mode_a, SocketAddr::V4(b.invite.addr), tag, random(1), 0).unwrap(),
            Connection::new(mode_b, SocketAddr::V4(a.invite.addr), tag, random(2), 0).unwrap(),
        ];
        World { net, local: local.map(SocketAddr::V4), conns, destinations: Default::default() }
    }

    /// One round: every connection sends what is due, then time moves to the next wake (at most
    /// `until`) and what arrives is handed over. Returns the new time.
    fn step(&mut self, until: u64) -> u64 {
        self.step_with_loss(until, |_, _| false)
    }

    fn step_with_loss(&mut self, until: u64, lose: impl FnMut(&Connection, &[u8]) -> bool) -> u64 {
        self.step_checked(until, lose, |_| {})
    }

    fn step_checked(&mut self, until: u64, mut lose: impl FnMut(&Connection, &[u8]) -> bool,
                    check: impl Fn(&Connection)) -> u64 {
        let now = self.net.now();
        for i in 0..2 {
            let datagrams = self.conns[i].poll(now);
            check(&self.conns[i]);
            for (to, bytes) in datagrams {
                self.destinations[i].push(to);
                if !lose(&self.conns[i], &bytes) {
                    self.net.send(self.local[i], to, bytes);
                }
            }
        }
        let wake = self.conns.iter().map(Connection::next_wake).chain(self.net.next_at()).filter(|t| *t > now).min().unwrap_or(until).min(until);
        for d in self.net.advance_to(wake) {
            let i = self.local.iter().position(|addr| *addr == d.to).expect("a delivery to one of the two sockets");
            assert!(Connection::is_ours(&d.bytes), "only punch and stream datagrams cross");
            self.conns[i].handle(d.from, &d.bytes, wake);
            check(&self.conns[i]);
        }
        wake
    }
}

fn linked(conns: &[Connection; 2]) -> bool {
    conns.iter().all(|c| *c.phase() == Phase::Linked)
}

fn linked_world() -> World {
    let f = fixture();
    let mut world = World::new(Routers::Separate, &f.a, &f.b, 7);
    while !linked(&world.conns) && world.step(30_000) < 30_000 {}
    assert!(linked(&world.conns), "both peers completed the proof");
    world
}

#[test]
fn both_devices_report_a_version_mismatch_under_loss() {
    let f = fixture();
    for older in 0..2 {
        let mut world = World::new(Routers::Separate, &f.a, &f.b, 7);
        world.conns[older].make_proof = Proof::version_one;
        let mut lost_at = None;
        while world.net.now() < 5_000 && !world.conns.iter().all(|c| c.done(world.net.now())) {
            let now = world.net.now();
            world.step_with_loss(5_000, |c, bytes| {
                matches!(c.phase(), Phase::Failed(_)) && bytes.first() == Some(&STREAM_KIND)
                    && *lost_at.get_or_insert(now) == now
            });
            assert!(world.conns.iter().all(|c| c.phase() != &Phase::Linked), "older side {older}: incompatible devices never link");
        }
        assert!(lost_at.is_some(), "older side {older}: the first refusal transmission was lost");
        for c in &world.conns {
            assert_eq!(c.phase(), &Phase::Failed(Failure::Refused(Refusal::Version)), "older side {older}: both devices explain the mismatch");
            assert!(c.done(world.net.now()), "older side {older}: the worker can stop within five virtual seconds");
        }
    }
}

#[test]
fn two_devices_link_through_their_nats_and_exchange_messages() {
    let f = fixture();
    let hairpin = Routers::Shared { hairpin: true };
    let no_hairpin = Routers::Shared { hairpin: false };
    for (topology, routers, a, b, expect_lan, seeds) in [
        ("two routers", Routers::Separate, &f.a, &f.b, false, &[7][..]),
        ("one router that hairpins", hairpin, &f.a, &f.b_same, false, &[7]),
        ("no hairpin, long invites", no_hairpin, &f.a_lan, &f.b_same_lan, true, &[7]),
        ("hairpin, long invites", hairpin, &f.a_lan, &f.b_same_lan, true, &[7, 8, 9]),
        ("two routers, long invites", Routers::Separate, &f.a_lan, &f.b_lan, false, &[7]),
    ] {
        for &seed in seeds {
            let case = format!("{topology}, seed {seed}");
            let mut world = World::new(routers, a, b, seed);
            let peers = [b, a];
            for (i, peer) in peers.iter().enumerate() {
                assert_eq!(world.conns[i].lan_probing(), if expect_lan { peer.invite.lan } else { None },
                    "{case}: side {i} reports the permitted LAN candidate");
            }
            exchange_messages(&mut world, &case, a, b);
            for (i, peer) in peers.iter().enumerate() {
                let expected = if expect_lan { peer.invite.lan.unwrap() } else { peer.invite.addr };
                assert_eq!(world.conns[i].peer_address(), Some(SocketAddr::V4(expected)), "{case}: side {i} uses the agreed path");
                if !expect_lan {
                    assert!(world.destinations[i].iter().all(|to| matches!(to.ip(), IpAddr::V4(ip) if !ip.is_private())),
                        "{case}: side {i} sent a datagram to a private address");
                }
            }
        }
    }
}

fn exchange_messages(world: &mut World, topology: &str, a: &Side, b: &Side) {
    while !linked(&world.conns) && world.step(30_000) < 30_000 {}
    let inside = |c: &Connection| format!("{:?} stream {:?} tls {:?} to_stream {} stream_id {:?} punch {:?}", c.phase(),
        c.stream.as_ref().map(|s| s.state()), c.tls.as_ref().map(|t| t.handshake_done()), c.to_stream.len(), c.stream_id, c.punch.state());
    assert!(linked(&world.conns), "{topology}: both sides linked within 30 s of virtual time: {} / {}",
        inside(&world.conns[0]), inside(&world.conns[1]));
    assert_eq!(world.conns[0].peer().map(|p| (p.device, p.name)), Some((b.device.public(), "Bob".to_string())), "{topology}: A knows B's device");
    assert_eq!(world.conns[1].peer().map(|p| (p.device, p.name)), Some((a.device.public(), "Ada".to_string())), "{topology}: B knows A's device");

    // Messages both ways, one small and one across many records and segments, under loss.
    let big: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
    world.conns[0].send(Kind::Sync, &big);
    world.conns[0].send(Kind::Call, b"hello");
    world.conns[1].send(Kind::Call, b"");
    world.conns[1].send(Kind::Sync, b"sync");
    let mut got = [Vec::new(), Vec::new()];
    let deadline = world.net.now() + 60_000;
    while !(got[1].len() == 2 && got[0].len() == 2) && world.step(deadline) < deadline {
        for i in 0..2 {
            got[i].extend(std::iter::from_fn(|| world.conns[i].recv()));
        }
    }
    assert!(linked(&world.conns), "{topology}: still linked after the messages");
    assert_eq!(got[1], [(Kind::Call, b"hello".to_vec()), (Kind::Sync, big)], "{topology}: B gets both kinds whole and in priority order");
    assert_eq!(got[0], [(Kind::Call, vec![]), (Kind::Sync, b"sync".to_vec())], "{topology}: A gets the empty call and the sync");
}

#[test]
fn the_link_caps_each_stream_and_refuses_an_unknown_kind() {
    let f = fixture();
    let mut world = World::new(Routers::Separate, &f.a, &f.b, 7);
    for c in &mut world.conns {
        assert_eq!(c.sent_bytes(), 0, "no stream has sent bytes before the punch");
        c.set_rate_cap(Some(8_000));
    }
    while !linked(&world.conns) && world.step(30_000) < 30_000 {}
    assert!(linked(&world.conns), "both capped streams complete the proof");
    let before = world.conns.each_ref().map(Connection::sent_bytes);
    let body = vec![9; 32 * 1024];
    for c in &mut world.conns {
        c.send(Kind::Sync, &body);
    }
    let deadline = world.net.now() + 3_000;
    while world.step(deadline) < deadline {}
    for (c, before) in world.conns.iter_mut().zip(before) {
        assert!(c.sent_bytes() > before, "sent bytes include message traffic");
        assert!(c.recv().is_none(), "the cap set before opening keeps 32 KiB from crossing in three seconds");
        c.set_rate_cap(None);
    }
    let deadline = world.net.now() + 30_000;
    while world.conns.iter().any(|c| c.incoming.inbox.is_empty()) && world.step(deadline) < deadline {}
    for c in &mut world.conns {
        assert_eq!(c.recv(), Some((Kind::Sync, body.clone())), "lifting the cap releases all queued bytes");
    }
    world.conns[0].to_tls.extend_from_slice(&[99, 0, 0]);
    let deadline = world.net.now() + 5_000;
    while !world.conns[1].done(world.net.now()) && world.step(deadline) < deadline {}
    assert_eq!(world.conns[1].phase(), &Phase::Failed(Failure::Refused(Refusal::Format("unknown message kind"))),
        "an unassigned wire kind fails the link");
}

#[test]
fn call_and_sync_interrupt_a_blob_and_each_kind_reassembles_in_order() {
    let mut world = linked_world();
    let deadline = world.net.now() + 30_000;
    let blob: Vec<u8> = (0..crate::blob::MAX_MESSAGE).map(|i| (i / FRAGMENT) as u8).collect();
    world.conns[0].send(Kind::Blob, &blob);
    while world.conns[1].incoming.partial[Kind::Blob.index()].is_empty() && world.step(deadline) < deadline {}
    assert!(!world.conns[1].incoming.partial[Kind::Blob.index()].is_empty(), "the first blob is partway across");

    let sync: Vec<u8> = (0..3 * FRAGMENT + 1).map(|i| (i / FRAGMENT + 3) as u8).collect();
    let second = vec![77; 2 * FRAGMENT];
    world.conns[0].send(Kind::Blob, &second);
    world.conns[0].send(Kind::Sync, &sync);
    world.conns[0].send(Kind::Sync, &second);
    world.conns[0].send(Kind::Call, b"ring");
    for (kind, expected) in [(Kind::Call, &b"ring"[..]), (Kind::Sync, &sync), (Kind::Sync, &second),
                             (Kind::Blob, &blob), (Kind::Blob, &second)] {
        while world.conns[1].incoming.inbox.is_empty() && world.step(deadline) < deadline {}
        let (got, bytes) = world.conns[1].recv().expect("each queued message arrives");
        assert_eq!(got, kind, "a higher priority message arrives before the interrupted blob");
        assert_eq!(bytes.as_slice(), expected, "fragments reassemble in order within {kind:?}");
        assert!(bytes.capacity() <= kind.maximum(), "reassembly stays inside the {kind:?} allocation bound");
    }

    let settle = world.net.now() + 1_000;
    while world.step(settle) < settle {}
    let rate = world.conns[0].rate;
    assert!(rate > 1_000, "held windows measured the uncapped transfer");
    let idle = settle + 1_000;
    while world.step(idle) < idle {}
    assert_eq!(world.conns[0].rate, rate, "idle windows preserve the measured capacity");
    world.conns[0].set_rate_cap(Some(8_000));
    assert_eq!(world.conns[0].rate, 1_000, "lowering the cap clamps the measurement immediately");
}

#[test]
fn the_lowest_cap_keeps_a_call_within_one_fragment_of_blob_traffic() {
    const CAP: u64 = 8_000;
    // One bulk record and one preempting Call record, including header, explicit nonce and tag.
    const TLS_OVERHEAD: usize = 2 * (5 + 8 + 16);
    let mut world = linked_world();
    world.net.set_link(LinkConfig { latency_ms: 20, jitter_ms: 5, loss_per_mille: 0 });
    for conn in &mut world.conns { conn.set_rate_cap(Some(CAP)); }
    let started = world.net.now();
    let deadline = started + 300_000;
    let blobs = [1, 2, 3].map(|byte| vec![byte; crate::blob::MAX_MESSAGE]);
    let (mut sent, mut received) = (0, 0);
    let mut call_at = None;
    let mut call_delay = None;
    while received < blobs.len() {
        let now = world.net.now();
        assert!(now < deadline, "three blobs drain at the lowest cap");
        if sent < blobs.len() && world.conns[0].writable(Kind::Blob) {
            world.conns[0].send(Kind::Blob, &blobs[sent]);
            assert!(!world.conns[0].writable(Kind::Blob), "the connection holds the current blob");
            sent += 1;
        }
        if call_at.is_none() && !world.conns[1].incoming.partial[Kind::Blob.index()].is_empty() {
            world.conns[0].send(Kind::Call, b"ring");
            call_at = Some(now);
        }
        let now = world.step_checked(deadline, |_, _| false, |conn| {
            assert!(conn.rate <= CAP / 8, "the measured rate respects the active cap");
            let bound = conn.mark() + FRAGMENT + FRAME_HEADER + TLS_OVERHEAD;
            assert!(conn.committed_unsent() <= bound, "committed bytes exceed mark + one frame + TLS overhead ({bound})");
        });
        while let Some((kind, bytes)) = world.conns[1].recv() {
            match kind {
                Kind::Call => {
                    assert_eq!(bytes, b"ring");
                    call_delay = Some(now - call_at.expect("call was queued"));
                }
                Kind::Blob => {
                    if received == 0 { assert!(now - started > 60_000, "one chunk takes over a minute"); }
                    assert_eq!(bytes, blobs[received], "each pulled blob arrives whole and in order");
                    received += 1;
                }
                Kind::Sync => panic!("unexpected Sync"),
            }
        }
    }
    assert!(call_delay.is_some_and(|ms| ms < 10_000), "Call send-to-receive delay was {call_delay:?} ms");
}

#[test]
fn malformed_fragments_are_refused_at_the_header() {
    let call = Kind::Call as u8;
    let mut over_call = Vec::new();
    for _ in 0..MAX_CALL / FRAGMENT {
        over_call.push(call | MORE);
        over_call.extend_from_slice(&(FRAGMENT as u16).to_be_bytes());
        over_call.resize(over_call.len() + FRAGMENT, 7);
    }
    over_call.extend_from_slice(&[call, 0, 1]); // Refuse before the extra byte arrives.
    for (mut raw, why, accepted) in [
        (over_call, "message too large", MAX_CALL),
        (vec![call | MORE, 0, 1], "short non-final fragment", 0),
        (vec![call, 0x10, 1], "fragment too large", 0),
    ] {
        let mut world = linked_world();
        let last = raw.pop().expect("a header length byte");
        world.conns[0].to_tls = raw;
        let deadline = world.net.now() + 30_000;
        while world.conns[1].incoming.header_len != 2 && world.step(deadline) < deadline {}
        assert_eq!(world.conns[1].incoming.header_len, 2, "a split header waits for its last byte");
        assert_eq!(world.conns[1].phase(), &Phase::Linked);
        world.conns[0].to_tls.push(last);
        while !world.conns[1].done(world.net.now()) && world.step(deadline) < deadline {}
        let peer = &world.conns[1];
        assert_eq!(peer.phase(), &Phase::Failed(Failure::Refused(Refusal::Format(why))));
        let partial = &peer.incoming.partial[Kind::Call.index()];
        assert_eq!(partial.len(), accepted, "the rejected fragment never extended the message");
        assert_eq!(partial.capacity(), accepted, "the rejected header caused no message allocation");
    }
}

#[test]
fn a_peers_claim_to_my_public_ip_cannot_enable_lan_probes() {
    let f = fixture();
    let mut world = World::new(Routers::Separate, &f.a, &f.b_same_lan, 7);
    assert_eq!(world.conns[0].lan_probing(), None, "my short invite disables the peer's LAN candidate");

    // Even a correctly tagged probe arriving directly from the claimed LAN address gets no reply.
    let mut peer = Punch::new(punch::Config::new(first_contact_tag(&mode(&f.a, &f.b_same_lan, "Ada")).unwrap()), world.local[0], random(2)[..8].try_into().unwrap(), 0);
    let (_, probe) = peer.poll(0).0.pop().expect("the first poll emits a probe");
    world.conns[0].handle(SocketAddr::V4(B_LAN), &probe, 0);
    while world.step(30_000) < 30_000 {}
    assert!(!world.destinations[0].is_empty(), "the short-invite peer exercised the punch");
    assert!(world.destinations[0].iter().all(|to| *to == SocketAddr::V4(f.b_same_lan.invite.addr)),
        "the short-invite peer sent a datagram outside the advertised public path");
}

#[test]
fn reconnect_tags_have_fixed_vectors_and_rotate_by_the_hour() {
    let secret = [0x11; 32];
    assert_eq!(probe_tag(&secret, 480_000), [0xf8, 0xcc, 0xd0, 0xa7, 0x72, 0xde, 0xc9, 0x88], "the tag of a fixed secret and hour");
    assert_eq!(probe_tag(&secret, 480_001), [0xca, 0x92, 0x9f, 0x21, 0xcb, 0x47, 0x01, 0x20], "the next hour's tag");
    assert_eq!(probe_tag(&secret, 0), [0x0a, 0xc7, 0xcf, 0x86, 0x89, 0xb3, 0x6e, 0x0b], "hour zero");
    assert_ne!(probe_tag(&[0x12; 32], 480_000), probe_tag(&secret, 480_000), "each pair has its own tags");
    let tags = ProbeTags::new(&secret, 480_000);
    assert_eq!((tags.send, tags.accept), (probe_tag(&secret, 480_000), [probe_tag(&secret, 479_999), probe_tag(&secret, 480_001)]),
        "an hour sends its own tag and accepts the neighbours'");
    assert_eq!(probe_hour(3_600 * 480_000 + 3_599), 480_000, "hours are UTC hours since the epoch");
    assert_eq!(probe_hour(3_600 * 480_001), 480_001, "and turn on the hour");
}

/// Two devices that pinned each other, reconnecting through their NATs with clocks `hours` apart.
fn reconnect_world(hours: [u64; 2], seed: u64) -> (World, [Arc<DeviceKey>; 2]) {
    let f = fixture();
    let devices = [f.a.device.clone(), f.b.device.clone()];
    let secret = devices[0].link_secret(&devices[1].public()).unwrap();
    let mut world = World::new(Routers::Separate, &f.a, &f.b, seed);
    let remembered = [Remembered { public: Some(f.b.invite.addr.into()), lan: None }, Remembered { public: Some(f.a.invite.addr.into()), lan: None }];
    for i in 0..2 {
        let mode = Mode::Reconnect(Reconnect { device: devices[i].clone(), peers: vec![Pin { device: devices[1 - i].public(), min_version: 1 }] });
        world.conns[i] = Connection::reconnect(mode, remembered[i], &ProbeTags::new(&secret, hours[i]), random(i as u8 + 5), 0).unwrap();
    }
    (world, devices)
}

#[test]
fn a_reconnect_links_without_words_across_the_turn_of_the_hour_and_not_two_hours_apart() {
    let (mut world, devices) = reconnect_world([480_000, 480_001], 11);
    while !linked(&world.conns) && world.step(30_000) < 30_000 {}
    assert!(linked(&world.conns), "devices an hour apart link with no words");
    assert_eq!(world.conns[0].peer().map(|p| p.device), Some(devices[1].public()), "A proved B's pinned key");
    assert_eq!(world.conns[1].peer().map(|p| p.device), Some(devices[0].public()), "B proved A's pinned key");
    world.conns[0].send(Kind::Sync, b"ping");
    world.conns[1].send(Kind::Sync, b"pong");
    let deadline = world.net.now() + 10_000;
    let mut got = [None, None];
    while got.iter().any(Option::is_none) && world.step(deadline) < deadline {
        for i in 0..2 {
            got[i] = got[i].take().or_else(|| world.conns[i].recv());
        }
    }
    assert_eq!(got, [Some((Kind::Sync, b"pong".to_vec())), Some((Kind::Sync, b"ping".to_vec()))], "a sync message crosses each way");
    for c in &world.conns {
        let path = c.path().expect("a linked connection has a path");
        assert!(path.public.is_some() && path.lan.is_none(), "a link over the public path records a public address: {path:?}");
    }

    let (mut world, _) = reconnect_world([480_000, 480_002], 11);
    let deadline = 60_000;
    while world.step(deadline) < deadline {}
    for (i, c) in world.conns.iter().enumerate() {
        assert!(matches!(c.phase(), Phase::Punching { .. }), "side {i}: two hours apart no probe is answered: {:?}", c.phase());
    }
}

#[test]
fn a_stranger_is_not_answered_and_the_pairs_probe_is_from_wherever_it_comes() {
    let (mut world, devices) = reconnect_world([480_000, 480_000], 12);
    let pair = probe_tag(&devices[0].link_secret(&devices[1].public()).unwrap(), 480_000);
    let stranger = probe_tag(&[0x77; 32], 480_000);
    let probe_under = |tag: [u8; 8]| {
        let mut punch = Punch::new(punch::Config::new(tag), "192.0.2.99:1".parse().unwrap(), [3; 8], 0);
        punch.poll(0).0.pop().unwrap().1
    };
    let from: SocketAddr = "198.51.100.77:9".parse().unwrap();
    world.conns[0].handle(from, &probe_under(stranger), 0);
    assert!(world.conns[0].poll(1).iter().all(|(to, _)| *to != from), "a probe under another device's tag gets no answer");
    world.conns[0].handle(from, &probe_under(pair), 2);
    assert!(world.conns[0].poll(3).iter().any(|(to, bytes)| *to == from && bytes[1] == 2),
        "a probe under the pair's tag is acknowledged from wherever it comes");
}

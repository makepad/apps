use super::*;
use makepad_network::p2p::natsim::{LinkConfig, NatConfig, NatKind, Net};
use std::net::Ipv4Addr;

const TOPIC: Topic = Topic::Opaque(7);

/// One socket and one port-restricted NAT per device. The driver supplies each new
/// punch's entropy and advances wall and monotonic time independently.
struct Mesh<const N: usize = 3> {
    net: Net,
    hubs: [Hub; N],
    keys: [PublicKey; N],
    local: [SocketAddr; N],
    public: [SocketAddr; N],
    seconds: u64,
    seeds: [[u8; N]; N],
    requests: [[usize; N]; N],
    linked: [[usize; N]; N],
    lost: [[usize; N]; N],
    sent: [[usize; N]; N],
    wire_bytes: [u64; N],
    full_batches: [usize; N],
    stalled: Option<(usize, usize)>,
    held_read: Option<(usize, usize)>,
    zero_credit: bool,
    sync: Vec<(usize, Option<Vec<u8>>)>,
    held_id: Option<[u8; 8]>,
    held: Vec<(usize, SocketAddr, Vec<u8>)>,
}

impl Mesh {
    fn new() -> Self {
        Self::with_roles(false)
    }

    fn with_roles(swap: bool) -> Self {
        Self::network(swap)
    }
}

impl<const N: usize> Mesh<N> {
    fn network(swap: bool) -> Self {
        let mut net = Net::new(42, LinkConfig { latency_ms: 1, jitter_ms: 0, loss_per_mille: 0 });
        let mut device = [(); N].map(|_| Arc::new(DeviceKey::generate().unwrap()));
        device.sort_by_key(|d| d.public());
        if swap {
            device.swap(0, N - 1);
        }
        let address = |n| SocketAddr::from((Ipv4Addr::new(192, 0, 2, n), 4000));
        let local = std::array::from_fn(|i| address(1 + i as u8));
        let public = std::array::from_fn(|i| address(101 + i as u8));
        for i in 0..N {
            let (SocketAddr::V4(inside), SocketAddr::V4(outside)) = (local[i], public[i]) else { unreachable!() };
            let router = net.add_nat(*outside.ip(), NatConfig::new(NatKind::PortRestricted));
            net.add_host(*inside.ip(), Some(router));
        }
        Self {
            keys: device.each_ref().map(|k| k.public()),
            hubs: device.map(Hub::new),
            net,
            local,
            public,
            seconds: 3600,
            seeds: std::array::from_fn(|i| std::array::from_fn(|j| if i == j { 0 } else { (10 * (i + 1) + j + usize::from(i > 0)) as u8 })),
            requests: [[0; N]; N],
            linked: [[0; N]; N],
            lost: [[0; N]; N],
            sent: [[0; N]; N],
            wire_bytes: [0; N],
            full_batches: [0; N],
            stalled: None, held_read: None, zero_credit: false, sync: Vec::new(),
            held_id: None,
            held: Vec::new(),
        }
    }
    fn now(&self) -> Now {
        Now { ms: self.net.now(), unix_secs: self.seconds + self.net.now() / 1000 }
    }
    fn key(&self, i: usize) -> Carrier {
        Carrier::Peer(self.keys[i])
    }
    fn wanted(&mut self, me: usize, peers: &[usize]) {
        let wanted = peers
            .iter()
            .map(|&i| {
                (
                    Pin { device: self.keys[i], min_version: crate::link::VERSION },
                    Remembered { public: Some(self.public[i]), lan: None },
                )
            })
            .collect::<Vec<_>>();
        let now = self.now();
        self.hubs[me].set_wanted(wanted, now);
    }
    fn all(&mut self) {
        for (i, peers) in [[1, 2], [0, 2], [0, 1]].iter().enumerate() {
            self.wanted(i, peers);
        }
    }
    fn turn(&mut self) {
        self.turn_after(5);
    }
    fn turn_after(&mut self, delta_ms: u64) {
        let arrivals = self.net.advance_to(self.net.now() + delta_ms);
        let now = self.now();
        let mut inboxes: [VecDeque<_>; N] = std::array::from_fn(|_| VecDeque::new());
        for d in arrivals {
            let i = self.local.iter().position(|a| *a == d.to).unwrap();
            inboxes[i].push_back(d);
        }
        for (i, mut inbox) in inboxes.into_iter().enumerate() {
            if let Some((reader, sender)) = self.stalled.filter(|(reader, _)| *reader == i) {
                // Drive the real signed transport, including ACKs, but leave its
                // application reader stopped until it advertises zero credit.
                let key = self.key(sender);
                let c = self.hubs[reader].slots.get_mut(&key).unwrap().conn.as_mut().unwrap();
                let stream = c.transport_mut().unwrap();
                for d in inbox {
                    stream.handle_datagram(now.ms, d.from, &d.bytes);
                }
                while let Some((to, bytes)) = stream.poll_transmit(now.ms) {
                    self.zero_credit |= bytes[24..26] == [0, 0];
                    self.net.send(self.local[reader], to, bytes);
                }
                continue;
            }
            self.hubs[i].begin_batch(now);
            loop {
                for d in inbox.drain(..inbox.len().min(4)) {
                    self.hubs[i].handle(d.from, &d.bytes, now.ms);
                }
                let batch = if inbox.is_empty() { BatchEnd::Drained } else { BatchEnd::MoreQueued };
                self.service(i, now, batch);
                if inbox.is_empty() {
                    break;
                }
                self.full_batches[i] += 1;
            }
        }
    }
    fn service(&mut self, i: usize, now: Now, batch: BatchEnd) {
        for Output { attempt, event } in self.hubs[i].poll(now, batch) {
            let remote = match attempt.carrier {
                Carrier::Words => None,
                Carrier::Peer(peer) => self.keys.iter().position(|k| *k == peer),
            };
            match event {
                Event::Punch => {
                    let mut seed = [100 + i as u8; 72];
                    if let Some(j) = remote {
                        seed[..8].fill(self.seeds[i][j]);
                        self.seeds[i][j] = self.seeds[i][j].wrapping_add(1);
                        self.requests[i][j] += 1;
                    }
                    self.hubs[i].punch(attempt, seed, now);
                }
                Event::Send(to, bytes) => {
                    let j = self.public.iter().position(|a| *a == to).unwrap();
                    self.sent[i][j] += 1;
                    if Connection::packet_stream_id(&bytes).is_some() {
                        self.wire_bytes[i] += bytes.len() as u64;
                        if self.held_read == Some((i, j)) { self.zero_credit |= bytes[24..26] == [0, 0]; }
                    }
                    if self.held_id.is_some() && Connection::packet_stream_id(&bytes) == self.held_id {
                        self.held.push((i, to, bytes));
                    } else {
                        self.net.send(self.local[i], to, bytes);
                    }
                }
                Event::Linked { peer, .. } => {
                    let j = remote.expect("Linked names the promoted carrier");
                    assert_eq!(self.keys[j], peer.device);
                    self.linked[i][j] += 1;
                }
                Event::Sync(bytes) => {
                    self.sync.push((i, Some(bytes)));
                    if self.held_read != Some((i, remote.unwrap())) { self.hubs[i].sync_read(attempt); }
                }
                Event::Lost { .. } => {
                    self.lost[i][remote.unwrap()] += 1;
                    self.sync.push((i, None));
                }
                Event::Failed(why) | Event::Refused(why) => panic!("carrier {attempt:?}: {why}"),
                _ => {}
            }
        }
    }
    fn until(&mut self, what: &str, mut reached: impl FnMut(&mut Self) -> bool) {
        for _ in 0..4000 {
            if reached(self) {
                return;
            }
            self.turn();
        }
        panic!("simulated turns exhausted waiting for {what}: linked {:?}, lost {:?}", self.linked, self.lost);
    }
    fn id(&self, i: usize, j: usize) -> [u8; 8] {
        self.hubs[i].slots[&self.key(j)].conn.as_ref().unwrap().stream_id().unwrap()
    }
    fn send(&mut self, i: usize, j: usize, bytes: &[u8]) {
        self.hubs[i].send_peer(self.keys[j], TOPIC, bytes.to_vec()).unwrap();
    }
    fn expect(&mut self, i: usize, j: usize, bytes: &[u8]) {
        self.until("authenticated marker", |m| {
            let messages = m.hubs[j].take_peer(TOPIC);
            if messages.is_empty() {
                return false;
            }
            assert_eq!(messages, [(m.keys[i], bytes.to_vec())]);
            true
        });
    }
}

#[test]
fn three_hubs_keep_each_pair_and_rotate_all_tags_without_relinking() {
    let mut m = Mesh::new();
    m.all();
    let full = [[0, 1, 1], [1, 0, 1], [1, 1, 0]];
    m.until("six authenticated endpoints", |m| m.linked == full);
    for i in 0..3 {
        assert_eq!(m.hubs[i].streams.len(), 2);
        for j in 0..3 {
            if i != j {
                assert_eq!(m.id(i, j), m.id(j, i));
                m.send(i, j, &[i as u8, j as u8]);
                m.expect(i, j, &[i as u8, j as u8]);
            }
        }
    }
    let ids = [m.id(0, 1), m.id(0, 2), m.id(1, 2)];
    m.all();
    let old = m.hubs[0].slots[&m.key(1)].tags.as_ref().unwrap().1;
    m.seconds += 3600;
    m.turn();
    for (tag, accepted) in [(old.send, true), (old.accept[0], false), (old.accept[1], true)] {
        let mut packet = [0; 18];
        packet[..2].copy_from_slice(&[makepad_network::p2p::punch::KIND, 1]);
        packet[2..10].copy_from_slice(&tag);
        packet[10..].fill(21); // B's original probe nonce.
        let now = m.now();
        m.hubs[0].handle(m.public[1], &packet, now.ms);
        let ack = m.hubs[0].poll(now,BatchEnd::Drained).into_iter().any(|o|
            matches!(o.event,Event::Send(to,b) if to == m.public[1] && Connection::punch_tag(&b)==Some(tag) && b[1]==2));
        assert_eq!(ack, accepted, "the new window answers exactly its own tags");
    }
    for (i, j) in [(0, 1), (1, 2), (2, 0)] {
        m.send(i, j, b"after rotation");
        m.expect(i, j, b"after rotation");
    }
    assert_eq!(ids, [m.id(0, 1), m.id(0, 2), m.id(1, 2)]);
    assert_eq!(m.requests, full);
    assert_eq!(m.linked, full);
    assert_eq!(m.lost, [[0; 3]; 3]);
}

#[test]
fn unwanted_probes_are_silent_through_removal_and_a_new_attempt_can_relink() {
    let mut m = Mesh::new();
    m.wanted(0, &[1]);
    m.wanted(1, &[0]);
    m.until("original link", |m| m.linked[0][1] == 1 && m.linked[1][0] == 1);
    let old_id = m.id(0, 1);
    let old_attempt = m.hubs[0].attempt(m.key(1)).unwrap();
    let old_tag = m.hubs[0].slots[&m.key(1)].tags.as_ref().unwrap().1.send;
    let unsolicited = |m: &mut Mesh, tag: [u8; 8], id: [u8; 8]| {
        let mut probe = [0; 18];
        probe[..2].copy_from_slice(&[0xe0, 1]);
        probe[2..10].copy_from_slice(&tag);
        let mut stream = [0; 32];
        stream[0] = 0xe1;
        stream[2..10].copy_from_slice(&id);
        let now = m.now();
        for bytes in [&probe[..], &stream, &[0xe1, 1], &[0xff], &[]] {
            m.hubs[0].handle(m.public[1], bytes, now.ms);
        }
        assert!(!m.hubs[0].poll(now, BatchEnd::Drained).iter().any(|o| matches!(o.event, Event::Send(..))));
    };
    let tag =
        connect::probe_tag(&m.hubs[2].device.link_secret(&m.keys[0]).unwrap(), connect::probe_hour(m.now().unix_secs));
    unsolicited(&mut m, tag, [99; 8]); // A real pair key, excluded from A's wanted snapshot.
    unsolicited(&mut m, [77; 8], [44; 8]);
    m.wanted(0, &[]);
    m.until("close completed", |m| m.hubs[0].phase(m.key(1)).is_none());
    let last = m.sent[0][1];
    unsolicited(&mut m, old_tag, old_id);
    m.turn_after(connect::RECONNECT_IDLE_INTERVAL_MS + 1);
    m.turn();
    m.turn();
    assert_eq!(m.sent[0][1], last, "no keepalive or probe after removal");
    m.wanted(0, &[1]);
    m.until("new link", |m| m.linked[0][1] == 2 && m.linked[1][0] == 2);
    assert_ne!(m.id(0, 1), old_id);
    assert_ne!(m.hubs[0].attempt(m.key(1)), Some(old_attempt));
    m.send(0, 1, b"back");
    m.expect(0, 1, b"back");
}

#[test]
fn a_busy_carrier_leaves_turns_for_another_carriers_marker() {
    let mut m = Mesh::new();
    m.all();
    m.until("full mesh", |m| m.linked == [[0, 1, 1], [1, 0, 1], [1, 1, 0]]);
    let marker_turns = |m: &mut Mesh| {
        let start = m.net.now();
        m.send(0, 2, b"marker");
        m.expect(0, 2, b"marker");
        (m.net.now() - start) / 5
    };
    let quiet = marker_turns(&mut m);
    // Reliable data from B fills A's socket batches at the same time as A's
    // outbound topic queue grows. C shares that socket in both directions.
    let before = m.full_batches[0];
    let ab = m.hubs[1].attempt(m.key(0)).unwrap();
    m.hubs[1].send_sync(ab, &vec![5; crate::sync::MAX_MESSAGE.min(64 * 1024)]);
    for _ in 0..128 {
        m.send(0, 1, &[3; 1024]);
    }
    m.until("an incoming B burst spanning socket batches", |m| m.full_batches[0] > before);
    let busy = marker_turns(&mut m);
    assert!(
        !m.hubs[0].slots[&m.key(1)].traffic.outgoing.is_empty(),
        "marker arrived while A–B still had queued traffic"
    );
    assert!(busy <= quiet + 8, "marker took {busy} simulated turns with backlog, {quiet} without");
}

#[test]
fn a_full_sync_inbox_stalls_only_its_own_stream_and_drains_in_order() {
    let mut m = Mesh::new();
    m.wanted(0, &[1, 2]);
    m.wanted(1, &[0]);
    m.wanted(2, &[0]);
    m.until("both carriers", |m| m.linked == [[0, 1, 1], [1, 0, 0], [1, 0, 0]]);
    let ben = m.hubs[0].attempt(m.key(1)).unwrap();
    let ana_at_ben = m.hubs[1].attempt(m.key(0)).unwrap();
    let ana_at_kit = m.hubs[2].attempt(m.key(0)).unwrap();
    let kit_line_and_signal = |m: &mut Mesh| {
        let start = m.net.now();
        m.hubs[2].send_sync(ana_at_kit, b"Kit's chat");
        m.hubs[2].send_peer(m.keys[0], Topic::Call, b"ring".to_vec()).unwrap();
        let mut rang = false;
        m.until("Kit's sync and call signal at Ana", |m| {
            for (peer, message) in m.hubs[0].take_peer(Topic::Call) {
                assert_eq!((peer, message.as_slice()), (m.keys[2], b"ring".as_slice()));
                rang = true;
            }
            rang && m.sync.iter().any(|(i, b)| *i == 0 && b.as_deref() == Some(b"Kit's chat"))
        });
        m.sync.retain(|(_, b)| b.as_deref() != Some(b"Kit's chat"));
        (m.net.now() - start) / 5
    };
    let quiet = kit_line_and_signal(&mut m);
    m.held_read = Some((0, 1));
    let records = 32;
    let body = |i| match i {
        0 => Vec::new(),
        1..=17 => vec![i],
        18 => vec![i; crate::sync::BATCH_BYTES + 1],
        _ => vec![i; 32 * 1024],
    };
    for i in 0..records {
        m.hubs[1].send_sync(ana_at_ben, &body(i));
    }
    m.until("Ben's stream advertises zero credit", |m| m.zero_credit);
    let busy = kit_line_and_signal(&mut m);
    assert_eq!(busy, quiet, "Kit's traffic takes the same simulated turns with Ben stalled");
    assert_eq!(m.sync.len(), 1, "Ana holds one message from Ben at a time");
    assert_eq!(m.hubs[0].sync_inboxes.credit(ben), 0);
    m.held_read = None;
    m.hubs[0].sync_read(ben);
    m.until("all of Ben's messages", |m| m.sync.len() == usize::from(records));
    for (i, (reader, bytes)) in m.sync.iter().enumerate() {
        assert_eq!(*reader, 0);
        assert!(bytes.as_ref() == Some(&body(i as u8)), "complete ordered message {i}");
    }
    assert!(!m.hubs[0].awaiting_sync());
    assert_eq!(m.lost, [[0; 3]; 3], "resuming requires no relink");
    eprintln!("Kit's chat and call signal: {quiet} quiet turns, {busy} turns at Ben's zero window");

    // The transport can finish a short tail and its FIN while the application is paused.
    m.sync.clear();
    m.held_read = Some((0, 1));
    for seq in 0..6 { m.hubs[1].send_sync(ana_at_ben, &vec![seq; 16 * 1024]); }
    m.wanted(1, &[]);
    m.until("one final batch at Ana", |m| m.sync.iter().any(|(i, body)| *i == 0 && body.is_some()));
    for _ in 0..(FAREWELL_MS / 5 + 1) { m.turn(); }
    assert_eq!(m.sync.iter().filter(|(i, _)| *i == 0).count(), 1, "Ana still holds her one credit");
    m.held_read = None;
    m.hubs[0].sync_read(ben);
    m.until("the final syncs followed by loss", |m| m.lost[0][1] == 1);
    let delivered: Vec<_> = m.sync.iter().filter(|(reader, _)| *reader == 0)
        .map(|(_, body)| body.as_ref().map(|b| b[0])).collect();
    assert_eq!(delivered, [Some(0), Some(1), Some(2), Some(3), Some(4), Some(5), None]);
}

#[test]
fn inbox_handover_round_robin() {
    use std::time::Instant;
    let mut hub = Hub::new(Arc::new(DeviceKey::generate().unwrap()));
    for count in [1, 2, 8, 32] {
        let peers: Vec<_> = (0..count).rev().map(|i| Attempt { carrier: Carrier::Peer([i; 65]), generation: 1 }).collect();
        let mut samples = Vec::new();
        for _ in 0..129 {
            for &at in &peers {
                hub.collect(at, vec![Event::Sync(vec![0]), Event::Sync(vec![1])]);
            }
            let start = Instant::now();
            let first = hub.take_events();
            for o in &first { hub.sync_read(o.attempt); }
            let elapsed = start.elapsed();
            let second = hub.take_events();
            for batch in [&first, &second] {
                assert!(batch.iter().map(|o| o.attempt).eq(peers.iter().copied()), "one message per ready peer in FIFO order");
            }
            assert!(first.iter().all(|o| matches!(&o.event, Event::Sync(b) if b == &[0])));
            assert!(second.iter().all(|o| matches!(&o.event, Event::Sync(b) if b == &[1])));
            assert!(hub.take_events().is_empty());
            for o in &second { hub.sync_read(o.attempt); }
            assert!(!hub.awaiting_sync());
            samples.push(elapsed);
        }
        samples.sort_unstable();
        eprintln!("inbox handover and credit return, {count} ready carriers: median {:?}, p95 {:?}", samples[64], samples[122]);
    }
    let old = Attempt { carrier: Carrier::Peer([1; 65]), generation: 1 };
    let new = Attempt { generation: 2, ..old };
    hub.collect(old, vec![Event::Sync(vec![0]), Event::Sync(vec![1]), Event::Lost { why: "closed".into(), sent_bytes: 0 }]);
    hub.collect(new, vec![Event::Sync(vec![2]), Event::Sync(vec![3])]);
    assert_eq!(hub.take_events().len(), 1);
    hub.sync_read(new);
    assert!(hub.take_events().is_empty(), "successor cannot acknowledge a retired inbox");
    hub.sync_read(old);
    assert!(matches!(hub.take_events().as_slice(), [Output { event: Event::Sync(_), .. }, Output { event: Event::Lost { .. }, .. }]));
    hub.sync_read(old);
    assert_eq!(hub.take_events()[0].attempt, new);
    hub.sync_read(old);
    assert!(hub.take_events().is_empty(), "late acknowledgement cannot spend the successor's credit");
    hub.sync_read(new);
    assert_eq!(hub.take_events().len(), 1);
    hub.sync_read(new);
    assert!(!hub.awaiting_sync());
}

#[test]
fn hub_routes_media_datagrams_for_four_attached_peers_and_keeps_shared_cap() {
    let mut mesh = Mesh::<5>::network(false);
    let peers = [1, 2, 3, 4];
    mesh.wanted(0, &peers);
    for i in peers {
        mesh.wanted(i, &[0]);
    }
    mesh.until("four authenticated carriers", |m| m.linked[0] == [0, 1, 1, 1, 1]);
    let attempts = peers.map(|i| mesh.hubs[0].attempt(mesh.key(i)).unwrap());
    let addresses = peers.map(|i| mesh.public[i]);
    let now = mesh.now();
    let hub = &mut mesh.hubs[0];
    for at in attempts {
        hub.attach_call(at, true);
    }
    let check_routes = |hub: &mut Hub, first: usize| {
        for (i, from) in addresses.into_iter().enumerate() {
            hub.handle(from, &[connect::CALL_DATAGRAM, i as u8], now.ms);
        }
        hub.handle(SocketAddr::from(([192, 0, 2, 200], 4000)), &[connect::CALL_DATAGRAM, 99], now.ms);
        let received: Vec<_> = hub.poll(now, BatchEnd::Drained).into_iter().filter_map(|out| match out.event {
            Event::Media(from, bytes) => Some((out.attempt, from, bytes)),
            _ => None,
        }).collect();
        let expected: Vec<_> = (first..attempts.len())
            .map(|i| (attempts[i], addresses[i], vec![connect::CALL_DATAGRAM, i as u8]))
            .collect();
        assert_eq!(received, expected);
    };
    check_routes(hub, 0);
    hub.set_call_cap(Some(100_000), now.ms);
    hub.attach_call(attempts[0], false);
    check_routes(hub, 1);
    assert_eq!(hub.pace.as_ref().map(|pace| pace.bps), Some(100_000));
    for &at in &attempts[1..] {
        hub.attach_call(at, false);
    }
    check_routes(hub, 4);
    assert!(hub.pace.is_none(), "the last detach releases the shared uplink budget");
}

#[test]
fn a_call_paces_the_sum_of_busy_carriers_including_retransmissions() {
    let mut m = Mesh::new();
    m.wanted(0, &[1, 2]);
    m.wanted(1, &[0]);
    m.wanted(2, &[0]);
    m.until("both carriers", |m| m.linked == [[0, 1, 1], [1, 0, 0], [1, 0, 0]]);
    let at = m.hubs[0].attempt(m.key(1)).unwrap();
    m.hubs[0].attach_call(at, true);
    // Both queues exist before the call reserves the uplink. Each nominal share
    // falls below the transport's individual pacer floor.
    for peer in [1, 2] {
        for _ in 0..128 { m.send(0, peer, &[7; 512]); }
    }
    let rate = 12_000;
    let start = m.net.now();
    let initial = m.wire_bytes[0];
    m.hubs[0].set_call_cap(Some(rate), start);
    m.net.set_link(LinkConfig { latency_ms: 1, jitter_ms: 0, loss_per_mille: 30 });
    let mut delivered = [0; 2];
    for _ in 0..1500 {
        m.turn_after(20);
        for i in 1..=2 {
            delivered[i - 1] += m.hubs[i].take_peer(TOPIC).len();
        }
        let sent = m.wire_bytes[0] - initial;
        assert_eq!(m.hubs[0].stream_bytes(), m.wire_bytes[0], "the call sees the entire uplink");
        let earned = rate * (m.net.now() - start) / 8000;
        assert!(sent <= earned + 2048, "all wire bytes share one budget: {sent} for {earned} earned");
    }
    assert!(delivered.iter().all(|&n| n > 10 && n < 128), "both backlogged carriers make progress: {delivered:?}");
}

#[test]
fn identical_nonce_pairs_restart_only_the_contender() {
    // The incumbent must survive regardless of which end opens the new stream.
    for swap in [false, true] {
        let mut m = Mesh::with_roles(swap);
        m.wanted(0, &[1]);
        m.wanted(1, &[0]);
        m.until("incumbent", |m| m.linked[0][1] == 1 && m.linked[1][0] == 1);
        let id = m.id(0, 1);
        m.seeds[0][2] = 11;
        m.seeds[2][0] = 21;
        m.wanted(0, &[1, 2]);
        m.wanted(2, &[0]);
        m.until("collision restart", |m| m.requests[0][2] == 2);
        assert_eq!(m.hubs[0].streams.get(&id), m.hubs[0].attempt(m.key(1)).as_ref());
        m.send(0, 1, b"still up");
        m.expect(0, 1, b"still up");
        m.until("contender proof", |m| m.linked[0][2] == 1 && m.linked[2][0] == 1);
        assert_ne!(m.id(0, 2), id);
        assert_eq!(m.requests[0][1], 1);
        assert_eq!(m.lost[0][1], 0);
        m.send(0, 2, b"fresh nonce");
        m.expect(0, 2, b"fresh nonce");
    }
}

#[test]
fn words_replace_one_carrier_while_its_older_stream_finishes() {
    let mut m = Mesh::new();
    m.wanted(0, &[1]);
    m.wanted(1, &[0]);
    m.until("older carrier", |m| m.linked[0][1] == 1 && m.linked[1][0] == 1);
    let id = m.id(0, 1);
    let old = m.hubs[0].attempt(m.key(1)).unwrap();
    let now = m.now();
    let invites = [0, 1].map(|i| {
        let at = m.hubs[i].start_words(false, now);
        let mut session = InviteSession::open(now.ms).unwrap();
        let key = session.key(now.ms).unwrap().public();
        let SocketAddr::V4(addr) = m.public[i] else { unreachable!() };
        let nat = Hint::Easy;
        let own = Invite { nat, addr, lan: None, commit: Invite::commitment(nat, addr, None, &key) };
        crate::link::remember(&own, &key, true);
        m.hubs[i].wait_for_words(at, session, own.clone(), None);
        (at, own)
    });
    m.held_id = Some(id);
    for i in [0, 1] {
        m.hubs[i].words(invites[i].0, invites[i].1.clone(), invites[1 - i].1.clone(), "peer".into(), now.ms);
    }
    m.until("both words promotions", |m| m.linked[0][1] == 2 && m.linked[1][0] == 2);
    let replacement = m.hubs[0].attempt(m.key(1)).unwrap();
    assert_ne!(replacement, old);
    assert_ne!(m.id(0, 1), id);
    m.until("both old FINs", |m| {
        [0, 1].into_iter().all(|i| m.held.iter().any(|(from, _, b)| *from == i && b[1] & 0x7f == 5))
    });
    for (i, to, bytes) in std::mem::take(&mut m.held) {
        m.net.send(m.local[i], to, bytes);
    }
    m.until("delayed old close ACK", |m| m.held.iter().any(|(_, _, b)| b[1] & 0x7f == 4));
    m.hubs[0].attach_call(replacement, true);
    m.hubs[0].attach_call(old, false);
    m.hubs[0].punch(old, [1; 72], now);
    assert_eq!(m.hubs[0].attached, BTreeSet::from([replacement]));
    m.send(0, 1, b"replacement");
    m.expect(0, 1, b"replacement");
    m.held_id = None;
    for (i, to, bytes) in std::mem::take(&mut m.held) {
        m.net.send(m.local[i], to, bytes);
    }
    m.until("old streams retired", |m| m.hubs[0].farewells.is_empty() && m.hubs[1].farewells.is_empty());
    assert_eq!(m.hubs[0].attempt(m.key(1)), Some(replacement));
    assert_eq!(m.hubs[0].streams.len(), 1);
    assert_eq!((m.lost[0][1], m.lost[1][0]), (1, 1), "only the displaced generation reported Lost");
    m.send(1, 0, b"after the ACK");
    m.expect(1, 0, b"after the ACK");
}

#[test]
fn a_shared_tag_delivers_the_probe_to_each_distinct_candidate() {
    let mut m = Mesh::new();
    m.wanted(0, &[1, 2]);
    m.turn();
    let now = m.now();
    let tag = [7; 8];
    let common = ProbeTags { send: tag, accept: [tag, tag], hour: 1 };
    let keys = [m.key(1), m.key(2)];
    let h = &mut m.hubs[0];
    for key in keys {
        let slot = h.slots.get_mut(&key).unwrap();
        slot.tags.as_mut().unwrap().1 = common;
        slot.conn.as_mut().unwrap().set_probe_tags(&common);
    }
    h.rebuild_tags();
    let mut probe = [0; 18];
    probe[..2].copy_from_slice(&[0xe0, 1]);
    probe[2..10].copy_from_slice(&tag);
    h.handle(m.public[2], &probe, now.ms);
    let acks: Vec<_> = h
        .poll(now, BatchEnd::Drained)
        .into_iter()
        .filter_map(|o| match o.event {
            Event::Send(to, b) if to == m.public[2] && Connection::punch_tag(&b) == Some(tag) && b[1] == 2 => {
                Some(o.attempt.carrier)
            }
            _ => None,
        })
        .collect();
    assert_eq!(acks.len(), 2);
    assert!(keys.iter().all(|k| acks.contains(k)));
    assert!(h.slots.values().all(|s| s.peer.is_none()), "tag fan-out grants no identity");
}

#[test]
fn retirement_delivers_the_removal_before_fin_or_expires_at_zero_credit() {
    use crate::event::{Actor, Chat, Content, Draft, Event as Record, Member, Via};
    use crate::fold::fold;
    use crate::log::{Log, tests::{admit, temp_dir}};
    use crate::sync::SyncEngine;

    for stalled in [false, true] {
        let mut m = Mesh::new();
        let (ana, ben) = (m.hubs[0].device.clone(), m.hubs[1].device.clone());
        let dir = temp_dir("mesh-farewell");
        let (_, genesis) = Record::found(&ana, 2, 0, 0, [0; 16]).unwrap();
        let mut log = Log::found(&dir.join("ana"), genesis.clone()).unwrap();
        let mut copy = Log::found(&dir.join("ben"), genesis).unwrap();
        admit(&mut log, &ana, &ben);
        copy.append_all(log.events()[1..].to_vec()).unwrap();
        let last = copy.events().last().unwrap().id;
        let seat = fold(copy.events()).seat_of(&ben.public()).unwrap();
        let append = |log: &mut Log, content: Content| {
            let bytes = content.encode();
            let draft = Draft { kind: content.kind(), kind_version: content.version(), critical: matches!(content, Content::Member(_)),
                actor: Actor::Human, via: Via::Ui, wall_ms: 0, parents: log.parents_for(&ana.public()), content: &bytes };
            let event = Record::create(&ana, log.town_id(), draft, [0; 16]).unwrap();
            let id = event.id;
            log.append(event).unwrap();
            id
        };
        let mut engine = SyncEngine::new();
        let mut receiver = SyncEngine::new();
        receiver.connect(&copy);
        engine.on_message(&receiver.poll_send(&copy).unwrap(), &mut log).unwrap();
        m.wanted(0, &[1, 2]);
        m.wanted(1, &[0]);
        m.wanted(2, &[0]);
        m.until("two authenticated carriers", |m| m.linked[0] == [0, 1, 1] && m.linked[1][0] == 1);
        let ben_slot = m.key(1);
        let at = m.hubs[0].attempt(ben_slot).unwrap();
        let tag = m.hubs[0].slots[&ben_slot].tags.as_ref().unwrap().1.send;
        if stalled {
            m.stalled = Some((1, 0));
            let data = append(&mut log, Content::Chat(Chat { text: "x".repeat(512 * 1024), ..Chat::default() }));
            engine.on_append(log.get(&data).unwrap());
            while let Some(bytes) = engine.poll_send(&log) { m.hubs[0].send_sync(at, &bytes); }
            m.until("a real zero receive window", |m| m.zero_credit);
        }
        let removal = append(&mut log, Content::Member(Member::Remove { seat, device: ben.public(), last }));
        let later = append(&mut log, Content::Chat(Chat { text: "after removal".into(), ..Chat::default() }));
        engine.on_append(log.get(&removal).unwrap());
        engine.on_append(log.get(&later).unwrap());
        engine.serve(&log, &fold(log.events()), &ana.public(), &ben.public());
        engine.finish_prefix(&log);
        while let Some(bytes) = engine.poll_send(&log) { m.hubs[0].send_sync(at, &bytes); }
        let until = m.now().ms + FAREWELL_MS;
        m.wanted(0, &[2]);
        assert!(m.hubs[0].tags.targets(&tag).is_empty());
        m.send(0, 2, b"still linked");
        m.expect(0, 2, b"still linked");
        if stalled {
            assert!(m.now().ms < until);
            m.turn_after(until - m.now().ms - 1);
            assert!(m.hubs[0].attempt(ben_slot).is_some());
            m.turn_after(1);
            assert!(m.hubs[0].attempt(ben_slot).is_none(), "zero credit cannot prolong the farewell");
            m.stalled = None;
            let ana_slot = m.key(0);
            m.hubs[1].slots.get_mut(&ana_slot).unwrap().wake = Some(0);
            m.turn(); // Deliver whatever Ben's transport already accepted to its real reader.
        } else {
            m.until("ordered FIN at Ben", |m| m.lost[1][0] == 1);
        }
        m.until("retirement", |m| m.hubs[0].attempt(ben_slot).is_none());
        let mut probe = vec![0xe0, 1];
        probe.extend(tag);
        probe.extend([88; 8]);
        let now = m.now();
        m.hubs[0].handle(m.public[1], &probe, now.ms);
        assert!(!m.hubs[0].poll(now, BatchEnd::Drained).iter().any(|o| matches!(&o.event, Event::Send(to, _) if *to == m.public[1])));
        for (_, bytes) in m.sync.iter().filter(|(i, _)| *i == 1) {
            match bytes {
                Some(bytes) => { receiver.on_message(bytes, &mut copy).unwrap(); }
                None => assert!(copy.get(&removal).is_some(), "removal must precede FIN"),
            }
        }
        assert!(copy.get(&later).is_none());
        if !stalled { assert!(copy.get(&removal).is_some()); }
        drop((copy, log));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

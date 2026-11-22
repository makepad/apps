//! The simulated network of a microsplash device: the far end of the USB
//! frame channel and, later, of the simulated radio. This peer terminates TCP
//! and WebSocket and bridges whole messages through events, so native and
//! browser builds use the same mechanism without host sockets in this crate.
//!
//! All state is deterministic and `Clone`, including timers, fault queues and
//! the seeded PRNG, for simulator rewind. Mutating calls take monotonically
//! nondecreasing virtual milliseconds; observations do not advance time.
//! `Stats` counts protocol refusals, malformed inputs, discarded traffic,
//! retransmissions and each injected fault, separately in both directions.
//!
//! No message is dropped or refused for its size. The 64 KiB reassembly buffer
//! of each connection (with its presence flags, about 128 KB) is the TCP
//! receive window, not a message limit. An incomplete message moves out of it
//! as it arrives in order, so a message of any size streams through. A
//! completed message holds window credit until `host_events` drains it: a host
//! that reads slowly holds the window shut, the device sees a zero window and
//! waits, and draining reopens it. Bytes sent beyond the advertised window are
//! not stored, only counted (`tcp_out_of_window`), and TCP retransmits them.
//! Outbound, `host_send` queues without a cap and `host_pending` reports the
//! backlog for the host to pace by.
//!
//! Call order breaks ties at the same timestamp. Timers and queued input are
//! serviced by every mutating call, including host calls. SplitMix64 supplies
//! both fault decisions and TCP initial sequence numbers. Reordered frames
//! wait for a successor, with a 40 ms fallback after their sampled delay.
//! Retries have no count limit; timeout backoff is capped at 60 s (or the
//! configured minimum if larger). HTTP and message queues have no size cap.

pub mod base64;
pub mod sha1;
pub mod tether;
pub mod wire;
mod fault;
mod link;
mod services;
mod tcp;
mod websocket;

use std::collections::BTreeMap;
use fault::{Link as FaultLink, Prng};
use wire::{checksum, ethernet, ipv4, transport_checksum, u16_at};
pub use link::{Link, LinkStats};
pub use tether::{Tether, TetherStats};
pub use websocket::accept as websocket_accept;

pub type ConnId = u64;

/// Defaults use documentation addresses, a one-hour lease, MTU 1500, no
/// endpoints/names/faults, and a 200 ms retransmission floor.
#[derive(Clone, Debug)]
pub struct Config {
    pub gateway_mac: [u8; 6],
    pub gateway_ip: [u8; 4],
    pub prefix_len: u8,
    pub device_ip: [u8; 4],
    pub lease_secs: u32,
    pub dns: Vec<(String, [u8; 4])>,
    pub endpoints: Vec<Endpoint>,
    pub mtu: u16,
    /// RFC 6298 estimator floor; 200 ms by default, with a 1 s initial RTO.
    pub rto_min_ms: u64,
    pub faults: Faults,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            gateway_mac: [2, 0, 0, 0, 0, 1], gateway_ip: [192, 0, 2, 1], prefix_len: 24,
            device_ip: [192, 0, 2, 2], lease_secs: 3600, dns: Vec::new(), endpoints: Vec::new(),
            mtu: 1500, rto_min_ms: 200, faults: Faults::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Endpoint { pub ip: [u8; 4], pub port: u16, pub id: u32 }

#[derive(Clone, Debug, Default)]
pub struct Faults {
    pub seed: u64,
    pub drop_ppm: u32,
    pub dup_ppm: u32,
    pub reorder_ppm: u32,
    pub delay_max_ms: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message { Text(String), Binary(Vec<u8>) }

impl Message {
    pub fn len(&self) -> usize { match self { Self::Text(v) => v.len(), Self::Binary(v) => v.len() } }
    pub fn is_empty(&self) -> bool { self.len() == 0 }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostEvent {
    Open { conn: ConnId, endpoint: u32, path: String },
    Message { conn: ConnId, msg: Message },
    Closed { conn: ConnId, code: u16, reason: String },
}

/// Drops/duplicates count original frames; delays count copies with a nonzero
/// sampled delay; reorders count frames selected for holding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FaultCounts { pub dropped: u64, pub duplicated: u64, pub reordered: u64, pub delayed: u64 }

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub frames_in: u64, pub frames_out: u64,
    pub faults_in: FaultCounts, pub faults_out: FaultCounts,
    pub ethernet_short: u64, pub ethernet_destination: u64, pub ethernet_type: u64,
    pub arp_malformed: u64, pub arp_ignored: u64, pub arp_replies: u64,
    pub ip_malformed: u64, pub ip_options: u64, pub ip_fragment: u64,
    pub ip_checksum: u64, pub ip_length: u64, pub ip_destination: u64, pub ip_protocol: u64,
    pub icmp_malformed: u64, pub icmp_checksum: u64, pub icmp_unsupported: u64, pub icmp_echoes: u64,
    pub udp_malformed: u64, pub udp_checksum: u64, pub udp_port: u64,
    pub dhcp_malformed: u64, pub dhcp_unsupported: u64, pub dhcp_other_server: u64,
    pub dhcp_offers: u64, pub dhcp_acks: u64, pub dhcp_naks: u64,
    pub dhcp_releases: u64, pub dhcp_expired: u64,
    pub dns_malformed: u64, pub dns_answers: u64, pub dns_nxdomain: u64, pub dns_no_data: u64,
    pub tcp_malformed: u64, pub tcp_checksum: u64, pub tcp_refused: u64, pub tcp_no_connection: u64,
    pub tcp_bad_ack: u64, pub tcp_unexpected_syn: u64, pub tcp_missing_ack: u64,
    pub tcp_out_of_window: u64, pub tcp_duplicate_bytes: u64, pub tcp_out_of_order: u64,
    pub tcp_resets: u64, pub tcp_invalid_reset: u64, pub tcp_retransmits: u64,
    pub tcp_fast_retransmits: u64, pub tcp_probes: u64, pub tcp_connections: u64,
    pub tcp_after_fin: u64, pub tcp_unsupported: u64,
    pub http_bad_request: u64, pub ws_opened: u64, pub ws_closed: u64,
    pub ws_unmasked: u64, pub ws_protocol: u64, pub ws_utf8: u64, pub ws_after_close_bytes: u64,
    pub ws_messages_in: u64, pub ws_bytes_in: u64, pub ws_messages_out: u64, pub ws_bytes_out: u64,
    pub host_unknown_conn: u64, pub host_not_open: u64, pub host_invalid_close: u64,
    pub device_resets: u64,
}

#[derive(Clone)]
pub struct Gateway {
    config: Config,
    now: u64,
    rng: Prng,
    incoming: FaultLink,
    outgoing: FaultLink,
    connections: BTreeMap<ConnId, tcp::Connection>,
    next_conn: ConnId,
    events: Vec<HostEvent>,
    device_mac: Option<[u8; 6]>,
    lease_until: Option<u64>,
    stats: Stats,
}

impl Gateway {
    /// Invalid configuration is a programmer error: prefix <= 32, MTU >= 576,
    /// positive lease/RTO floor, and probabilities <= 1,000,000 ppm are required.
    /// No I/O or runtime setup is performed.
    pub fn new(config: Config) -> Self {
        assert!(config.prefix_len <= 32 && config.mtu >= 576 && config.rto_min_ms > 0 && config.lease_secs > 0);
        assert!(config.faults.drop_ppm <= 1_000_000 && config.faults.dup_ppm <= 1_000_000 && config.faults.reorder_ppm <= 1_000_000);
        Self {
            rng: Prng::new(config.faults.seed), config, now: 0, incoming: FaultLink::default(),
            outgoing: FaultLink::default(), connections: BTreeMap::new(), next_conn: 1,
            events: Vec::new(), device_mac: None, lease_until: None, stats: Stats::default(),
        }
    }

    pub fn frame_in(&mut self, now_ms: u64, frame: &[u8]) {
        self.advance(now_ms);
        self.stats.frames_in += 1;
        self.incoming.inject(now_ms, frame.to_vec(), &self.config.faults, &mut self.rng, &mut self.stats.faults_in);
        self.advance(now_ms);
    }

    pub fn frames_out(&mut self, now_ms: u64) -> Vec<Vec<u8>> {
        self.advance(now_ms);
        let frames = self.outgoing.take(now_ms);
        self.stats.frames_out += frames.len() as u64;
        frames
    }

    /// Drain events and return their receive-window credit to each connection.
    pub fn host_events(&mut self, now_ms: u64) -> Vec<HostEvent> {
        self.advance(now_ms);
        let ids: Vec<_> = self.connections.keys().copied().collect();
        for id in ids {
            let mut conn = self.connections.remove(&id).unwrap();
            conn.host_drained();
            conn.tick(now_ms, &mut self.stats);
            self.collect(conn);
        }
        std::mem::take(&mut self.events)
    }

    pub fn host_send(&mut self, now_ms: u64, conn: ConnId, msg: Message) {
        self.advance(now_ms);
        if let Some(mut connection) = self.connections.remove(&conn) {
            connection.host_send(msg, &mut self.stats);
            connection.tick(now_ms, &mut self.stats);
            self.collect(connection);
        } else { self.stats.host_unknown_conn += 1; }
    }

    /// Queue a close frame followed by a TCP half-close, retaining the receive
    /// half for the peer's close. Invalid codes/reasons are counted and refused.
    pub fn host_close(&mut self, now_ms: u64, conn: ConnId, code: u16, reason: &str) {
        self.advance(now_ms);
        if !websocket::valid_code(code) || reason.len() > 123 { self.stats.host_invalid_close += 1; return; }
        if let Some(mut connection) = self.connections.remove(&conn) {
            connection.host_close(code, reason, &mut self.stats);
            connection.tick(now_ms, &mut self.stats);
            self.collect(connection);
        } else { self.stats.host_unknown_conn += 1; }
    }

    /// The device reset (a new Hello on its link). Frames still in flight either way are stale and
    /// dropped; every connection closes for the host with code 1006 "device reset"; the lease and
    /// the learned MAC are forgotten, so the restarted stack begins again with DHCP.
    pub fn device_reset(&mut self, now_ms: u64) {
        self.advance(now_ms);
        self.stats.device_resets += 1;
        self.incoming.clear();
        self.outgoing.clear();
        for (_, mut conn) in std::mem::take(&mut self.connections) {
            conn.device_reset(&mut self.stats);
            self.events.append(&mut conn.events);
        }
        self.lease_until = None;
        self.device_mac = None;
    }

    /// Queued plus unacknowledged wire bytes, including HTTP/WS framing.
    pub fn host_pending(&self, conn: ConnId) -> usize { self.connections.get(&conn).map_or(0, |c| c.pending()) }

    pub fn next_deadline(&self) -> Option<u64> {
        self.incoming.deadline().into_iter().chain(self.outgoing.deadline())
            .chain(self.lease_until).chain(self.connections.values().filter_map(|c| c.deadline())).min()
    }

    pub fn stats(&self) -> Stats { self.stats }

    fn advance(&mut self, now: u64) {
        assert!(now >= self.now, "virtual time must not run backwards; restore a clone to rewind");
        self.now = now;
        if self.lease_until.is_some_and(|at| at <= now) { self.lease_until = None; self.stats.dhcp_expired += 1; }
        for frame in self.incoming.take(now) { self.receive(&frame); }
        let ids: Vec<_> = self.connections.keys().copied().collect();
        for id in ids {
            let mut conn = self.connections.remove(&id).unwrap();
            conn.tick(now, &mut self.stats);
            self.collect(conn);
        }
    }

    fn collect(&mut self, mut conn: tcp::Connection) {
        for segment in conn.output.drain(..) {
            self.send_ip(conn.mac, conn.local_ip, conn.remote_ip, 6, &segment);
        }
        self.events.append(&mut conn.events);
        if !conn.done { self.connections.insert(conn.id, conn); }
    }

    fn send(&mut self, frame: Vec<u8>) {
        self.outgoing.inject(self.now, frame, &self.config.faults, &mut self.rng, &mut self.stats.faults_out);
    }

    fn send_ip(&mut self, mac: [u8; 6], src: [u8; 4], dst: [u8; 4], proto: u8, payload: &[u8]) {
        self.send(ethernet(self.config.gateway_mac, mac, 0x0800, &ipv4(src, dst, proto, payload)));
    }

    fn receive(&mut self, frame: &[u8]) {
        if frame.len() < 14 { self.stats.ethernet_short += 1; return; }
        if frame[..6] != self.config.gateway_mac && frame[..6] != [255; 6] {
            self.stats.ethernet_destination += 1; return;
        }
        let mac: [u8; 6] = frame[6..12].try_into().unwrap();
        self.device_mac = Some(mac);
        match u16_at(frame, 12) {
            0x0806 => self.arp(mac, &frame[14..]),
            0x0800 => self.receive_ip(mac, &frame[14..]),
            _ => self.stats.ethernet_type += 1,
        }
    }

    fn receive_ip(&mut self, mac: [u8; 6], packet: &[u8]) {
        if packet.len() < 20 || packet[0] >> 4 != 4 || packet[0] & 15 < 5 { self.stats.ip_malformed += 1; return; }
        if packet[0] & 15 != 5 { self.stats.ip_options += 1; return; }
        let length = usize::from(u16_at(packet, 2));
        if length < 20 || length > packet.len() { self.stats.ip_length += 1; return; }
        if checksum(&packet[..20]) != 0 { self.stats.ip_checksum += 1; return; }
        if u16_at(packet, 6) & 0x3fff != 0 { self.stats.ip_fragment += 1; return; }
        if u16_at(packet, 6) & 0x8000 != 0 { self.stats.ip_malformed += 1; return; }
        let src = packet[12..16].try_into().unwrap();
        let dst = packet[16..20].try_into().unwrap();
        let body = &packet[20..length];
        match packet[9] {
            6 => self.receive_tcp(mac, src, dst, body),
            17 => self.udp(mac, src, dst, body),
            1 if dst == self.config.gateway_ip => {
                if body.len() < 8 { self.stats.icmp_malformed += 1; }
                else if checksum(body) != 0 { self.stats.icmp_checksum += 1; }
                else if body[0] != 8 || body[1] != 0 { self.stats.icmp_unsupported += 1; }
                else {
                    let mut reply = body.to_vec();
                    reply[0] = 0;
                    wire::put16(&mut reply, 2, 0);
                    let check = checksum(&reply);
                    wire::put16(&mut reply, 2, check);
                    self.stats.icmp_echoes += 1;
                    self.send_ip(mac, dst, src, 1, &reply);
                }
            }
            1 => self.stats.ip_destination += 1,
            _ => self.stats.ip_protocol += 1,
        }
    }

    fn receive_tcp(&mut self, mac: [u8; 6], src: [u8; 4], dst: [u8; 4], body: &[u8]) {
        if body.len() < 20 { self.stats.tcp_malformed += 1; return; }
        if transport_checksum(src, dst, 6, body) != 0 { self.stats.tcp_checksum += 1; return; }
        let Some(segment) = tcp::Segment::parse(body) else { self.stats.tcp_malformed += 1; return; };
        let id = self.connections.values().find(|c| c.matches(src, dst, segment.src, segment.dst)).map(|c| c.id);
        if let Some(id) = id {
            let mut conn = self.connections.remove(&id).unwrap();
            conn.mac = mac;
            conn.receive(self.now, &segment, &mut self.stats);
            conn.tick(self.now, &mut self.stats);
            self.collect(conn);
        } else if segment.flags & tcp::RST != 0 { self.stats.tcp_no_connection += 1; }
        else {
            let endpoint = self.config.endpoints.iter().find(|e| e.ip == dst && e.port == segment.dst);
            if segment.flags & !0xc0 == tcp::SYN && segment.payload.is_empty() && endpoint.is_some() {
                let endpoint = endpoint.unwrap().id;
                let id = self.next_conn;
                self.next_conn = self.next_conn.checked_add(1).expect("connection identifiers exhausted");
                let conn = tcp::Connection::new(id, endpoint, mac, src, dst, &segment, self.rng.next() as u32, self.now, &self.config);
                self.stats.tcp_connections += 1;
                self.collect(conn);
            } else {
                if endpoint.is_none() { self.stats.tcp_refused += 1; }
                else { self.stats.tcp_no_connection += 1; }
                self.send_ip(mac, dst, src, 6, &tcp::reset(src, dst, &segment));
            }
        }
    }
}

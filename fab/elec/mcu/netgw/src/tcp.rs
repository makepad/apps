use std::collections::VecDeque;
use crate::{Config, ConnId, HostEvent, Message, Stats, websocket};
use crate::wire::{put16, put32, transport_checksum, u16_at, u32_at};

pub(crate) const FIN: u8 = 1;
pub(crate) const SYN: u8 = 2;
pub(crate) const RST: u8 = 4;
pub(crate) const ACK: u8 = 16;
const CAPACITY: usize = 65535;

fn before(a: u32, b: u32) -> bool { (a.wrapping_sub(b) as i32) < 0 }

pub(crate) struct Segment<'a> {
    pub src: u16, pub dst: u16, pub seq: u32, pub ack: u32,
    pub flags: u8, pub window: u16, pub mss: Option<u16>, pub payload: &'a [u8],
}

impl<'a> Segment<'a> {
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        let header = usize::from(*bytes.get(12)? >> 4) * 4;
        if header < 20 || header > bytes.len() || bytes[12] & 15 != 0 { return None; }
        let mut at = 20;
        let mut mss = None;
        while at < header {
            let kind = bytes[at];
            if kind == 0 { break; }
            if kind == 1 { at += 1; continue; }
            if at + 1 >= header { return None; }
            let len = usize::from(bytes[at + 1]);
            if len < 2 || at + len > header { return None; }
            if kind == 2 {
                if len != 4 || mss.is_some() || u16_at(bytes, at + 2) == 0 { return None; }
                mss = Some(u16_at(bytes, at + 2));
            }
            at += len;
        }
        Some(Self {
            src: u16_at(bytes, 0), dst: u16_at(bytes, 2), seq: u32_at(bytes, 4), ack: u32_at(bytes, 8),
            flags: bytes[13], window: u16_at(bytes, 14), mss, payload: &bytes[header..],
        })
    }

    fn len(&self) -> u32 { self.payload.len() as u32 + u32::from(self.flags & SYN != 0) + u32::from(self.flags & FIN != 0) }
}

fn encode(src: [u8; 4], dst: [u8; 4], ports: (u16, u16), seq: u32, ack: u32, flags: u8, window: u16, mss: u16, data: &[u8]) -> Vec<u8> {
    let mut out = vec![0; if flags & SYN != 0 { 24 } else { 20 }];
    put16(&mut out, 0, ports.0);
    put16(&mut out, 2, ports.1);
    put32(&mut out, 4, seq);
    put32(&mut out, 8, ack);
    out[12] = ((out.len() / 4) as u8) << 4;
    out[13] = flags;
    put16(&mut out, 14, window);
    if flags & SYN != 0 { out[20..24].copy_from_slice(&[2, 4, (mss >> 8) as u8, mss as u8]); }
    out.extend_from_slice(data);
    let check = transport_checksum(src, dst, 6, &out);
    put16(&mut out, 16, check);
    out
}

pub(crate) fn reset(src: [u8; 4], dst: [u8; 4], segment: &Segment<'_>) -> Vec<u8> {
    let (seq, ack, flags) = if segment.flags & ACK != 0 { (segment.ack, 0, RST) }
        else { (0, segment.seq.wrapping_add(segment.len()), RST | ACK) };
    encode(dst, src, (segment.dst, segment.src), seq, ack, flags, 0, 0, &[])
}

#[derive(Clone)]
struct Flight {
    seq: u32, flags: u8, data: Vec<u8>, sent: u64, retransmitted: bool,
}

impl Flight {
    fn len(&self) -> u32 { self.data.len() as u32 + u32::from(self.flags & (SYN | FIN) != 0) }
    fn end(&self) -> u32 { self.seq.wrapping_add(self.len()) }
}

#[derive(Clone)]
pub(crate) struct Connection {
    pub id: ConnId,
    endpoint: u32,
    pub mac: [u8; 6],
    pub local_ip: [u8; 4],
    pub remote_ip: [u8; 4],
    local_port: u16,
    remote_port: u16,
    pub output: Vec<Vec<u8>>,
    pub events: Vec<HostEvent>,
    pub done: bool,
    opened: bool,
    notified: bool,
    established: bool,
    peer_syn: u32,
    snd_una: u32,
    snd_nxt: u32,
    wl1: u32,
    wl2: u32,
    peer_window: u32,
    mss: u32,
    local_mss: u16,
    cwnd: u32,
    ssthresh: u32,
    ca_bytes: u32,
    duplicate_acks: u32,
    recovery: Option<u32>,
    queue: VecDeque<u8>,
    flight: VecDeque<Flight>,
    rto_min: u64,
    rto: u64,
    srtt: Option<u64>,
    rttvar: u64,
    retransmit_at: Option<u64>,
    probe_at: Option<u64>,
    probe_interval: u64,
    rcv_nxt: u32,
    received: Vec<u8>,
    present: Vec<bool>,
    used: usize,
    host_credit: usize,
    ack_at: Option<u64>,
    ack_count: u8,
    ack_now: bool,
    peer_fin_seq: Option<u32>,
    peer_fin: bool,
    fin_wanted: bool,
    fin_sent: bool,
    fin_acked: bool,
    passive_close: bool,
    time_wait: Option<u64>,
    ws: websocket::WebSocket,
}

impl Connection {
    pub fn new(id: ConnId, endpoint: u32, mac: [u8; 6], remote_ip: [u8; 4], local_ip: [u8; 4], syn: &Segment<'_>, isn: u32, now: u64, config: &Config) -> Self {
        let mss = u32::from(syn.mss.unwrap_or(536).min(config.mtu - 40));
        let mut c = Self {
            id, endpoint, mac, local_ip, remote_ip, local_port: syn.dst, remote_port: syn.src,
            output: Vec::new(), events: Vec::new(), done: false, opened: false, notified: false,
            established: false, peer_syn: syn.seq, snd_una: isn, snd_nxt: isn, wl1: syn.seq, wl2: 0,
            peer_window: u32::from(syn.window), mss, local_mss: config.mtu - 40,
            cwnd: (4 * mss).min((2 * mss).max(4380)), ssthresh: 65535, ca_bytes: 0,
            duplicate_acks: 0, recovery: None, queue: VecDeque::new(), flight: VecDeque::new(),
            rto_min: config.rto_min_ms, rto: config.rto_min_ms.max(1000), srtt: None, rttvar: 0,
            retransmit_at: None, probe_at: None, probe_interval: config.rto_min_ms,
            rcv_nxt: syn.seq.wrapping_add(1), received: vec![0; 65536], present: vec![false; 65536],
            used: 0, host_credit: 0, ack_at: None, ack_count: 0, ack_now: false,
            peer_fin_seq: None, peer_fin: false, fin_wanted: false, fin_sent: false,
            fin_acked: false, passive_close: false, time_wait: None, ws: websocket::WebSocket::default(),
        };
        c.transmit(now, SYN | ACK, Vec::new());
        c
    }

    pub fn matches(&self, src: [u8; 4], dst: [u8; 4], src_port: u16, dst_port: u16) -> bool {
        self.remote_ip == src && self.local_ip == dst && self.remote_port == src_port && self.local_port == dst_port
    }

    fn window(&self) -> u16 { CAPACITY.saturating_sub(self.used.saturating_add(self.host_credit)) as u16 }

    fn emit(&mut self, seq: u32, flags: u8, bytes: &[u8]) {
        self.output.push(encode(self.local_ip, self.remote_ip, (self.local_port, self.remote_port), seq, self.rcv_nxt,
            flags, self.window(), self.local_mss, bytes));
        self.ack_at = None;
        self.ack_count = 0;
        self.ack_now = false;
    }

    fn transmit(&mut self, now: u64, flags: u8, data: Vec<u8>) {
        let flight = Flight { seq: self.snd_nxt, flags, data, sent: now, retransmitted: false };
        self.emit(flight.seq, flags, &flight.data);
        self.snd_nxt = self.snd_nxt.wrapping_add(flight.len());
        self.flight.push_back(flight);
        self.retransmit_at.get_or_insert(now.saturating_add(self.rto));
    }

    fn retransmit(&mut self, now: u64, stats: &mut Stats) {
        if let Some(flight) = self.flight.front_mut() {
            flight.retransmitted = true;
            flight.sent = now;
            let (seq, flags, data) = (flight.seq, flight.flags, flight.data.clone());
            self.emit(seq, flags, &data);
            self.retransmit_at = Some(now.saturating_add(self.rto));
            stats.tcp_retransmits += 1;
        }
    }

    fn closed(&mut self, code: u16, reason: String, stats: &mut Stats) {
        if self.opened && !self.notified {
            self.events.push(HostEvent::Closed { conn: self.id, code, reason });
            self.notified = true;
            stats.ws_closed += 1;
        }
    }

    pub fn host_send(&mut self, message: Message, stats: &mut Stats) {
        if !self.ws.is_open() || self.fin_wanted { stats.host_not_open += 1; return; }
        stats.ws_messages_out += 1;
        stats.ws_bytes_out += message.len() as u64;
        let data = match message {
            Message::Text(text) => websocket::frame(1, text.as_bytes()),
            Message::Binary(bytes) => websocket::frame(2, &bytes),
        };
        self.queue.extend(data);
    }

    pub fn host_close(&mut self, code: u16, reason: &str, stats: &mut Stats) {
        if !self.ws.is_open() || self.fin_wanted { stats.host_not_open += 1; return; }
        self.queue.extend(websocket::close_frame(code, reason));
        self.ws.host_close();
        self.fin_wanted = true;
    }

    /// The device restarted and its stack forgot this connection: the host hears it closed.
    pub fn device_reset(&mut self, stats: &mut Stats) {
        self.closed(1006, "device reset".into(), stats);
        self.done = true;
    }

    pub fn host_drained(&mut self) {
        if self.host_credit != 0 {
            self.host_credit = 0;
            self.ack_now = true;
        }
    }

    pub fn pending(&self) -> usize { self.queue.len() + self.flight.iter().map(|f| f.data.len()).sum::<usize>() }

    pub fn deadline(&self) -> Option<u64> {
        self.retransmit_at.into_iter().chain(self.probe_at).chain(self.ack_at).chain(self.time_wait).min()
    }

    pub fn receive(&mut self, now: u64, s: &Segment<'_>, stats: &mut Stats) {
        if s.flags & RST != 0 {
            if s.seq == self.rcv_nxt {
                stats.tcp_resets += 1;
                self.closed(1006, "TCP reset".into(), stats);
                self.done = true;
            } else { stats.tcp_invalid_reset += 1; self.ack_now = true; }
            return;
        }
        if self.time_wait.is_some() {
            self.ack_now = true;
            if s.flags & FIN != 0 && s.seq.wrapping_add(s.len()) == self.rcv_nxt {
                self.time_wait = Some(now.saturating_add(1000));
            } else { stats.tcp_after_fin += 1; }
            return;
        }
        if !self.established && s.flags & !0xc0 == SYN && s.seq == self.peer_syn {
            self.retransmit(now, stats);
            return;
        }
        if s.flags & SYN != 0 { stats.tcp_unexpected_syn += 1; self.ack_now = true; return; }
        if s.flags & 0x20 != 0 { stats.tcp_unsupported += 1; self.ack_now = true; return; }
        if s.flags & ACK == 0 { stats.tcp_missing_ack += 1; return; }
        let offset = s.seq.wrapping_sub(self.rcv_nxt) as i32;
        let length = s.len();
        let window = i64::from(self.window());
        let acceptable = if length == 0 {
            offset == 0 || (offset > 0 && i64::from(offset) < window)
        } else {
            window > 0 && i64::from(offset) < window && i64::from(offset) + i64::from(length) > 0
        };
        if !acceptable {
            stats.tcp_out_of_window += 1;
            self.ack_now = true;
            return;
        }
        if before(self.snd_nxt, s.ack) { stats.tcp_bad_ack += 1; self.ack_now = true; return; }
        if !self.established {
            if s.ack != self.snd_nxt {
                stats.tcp_bad_ack += 1;
                self.output.push(reset(self.remote_ip, self.local_ip, s));
                return;
            }
            self.established = true;
        }
        let old_window = self.peer_window;
        if !before(s.ack, self.snd_una) && (!before(s.seq, self.wl1) && (s.seq != self.wl1 || !before(s.ack, self.wl2))) {
            self.peer_window = u32::from(s.window);
            self.wl1 = s.seq;
            self.wl2 = s.ack;
        }
        if before(self.snd_una, s.ack) { self.acknowledge(now, s.ack, stats); }
        else if s.ack == self.snd_una && !self.flight.is_empty() && s.payload.is_empty()
            && s.flags & FIN == 0 && old_window == self.peer_window && self.peer_window != 0 {
            self.duplicate_acks = self.duplicate_acks.saturating_add(1);
            if self.duplicate_acks == 3 {
                self.ssthresh = (self.snd_nxt.wrapping_sub(self.snd_una) / 2).max(2 * self.mss);
                self.cwnd = self.ssthresh.saturating_add(3 * self.mss);
                self.recovery = Some(self.snd_nxt);
                stats.tcp_fast_retransmits += 1;
                self.retransmit(now, stats);
            } else if self.duplicate_acks > 3 && self.recovery.is_some() {
                self.cwnd = self.cwnd.saturating_add(self.mss);
            }
        } else { self.duplicate_acks = 0; }
        if length != 0 { self.receive_data(now, s, stats); }
        self.finish(now);
    }

    fn acknowledge(&mut self, now: u64, ack: u32, stats: &mut Stats) {
        let acknowledged = ack.wrapping_sub(self.snd_una);
        let ambiguous = self.flight.iter().take_while(|f| before(f.seq, ack)).any(|f| f.retransmitted);
        let mut sample = None;
        while self.flight.front().is_some_and(|f| !before(ack, f.end())) {
            let flight = self.flight.pop_front().unwrap();
            if !ambiguous { sample = Some(now.saturating_sub(flight.sent).max(1)); }
            if flight.flags & FIN != 0 { self.fin_acked = true; }
        }
        if let Some(flight) = self.flight.front_mut() {
            if before(flight.seq, ack) {
                let n = ack.wrapping_sub(flight.seq) as usize;
                flight.data.drain(..n);
                flight.seq = ack;
            }
        }
        if let Some(rtt) = sample {
            if let Some(srtt) = self.srtt {
                self.rttvar = ((3 * u128::from(self.rttvar) + u128::from(srtt.abs_diff(rtt))) / 4) as u64;
                self.srtt = Some(((7 * u128::from(srtt) + u128::from(rtt)) / 8) as u64);
            } else { self.srtt = Some(rtt); self.rttvar = rtt / 2; }
            self.rto = self.rto_min.max(self.srtt.unwrap().saturating_add(self.rttvar.saturating_mul(4).max(1)));
        }
        self.snd_una = ack;
        self.duplicate_acks = 0;
        self.retransmit_at = if self.flight.is_empty() { None } else { Some(now.saturating_add(self.rto)) };
        if let Some(recover) = self.recovery {
            if before(ack, recover) {
                self.cwnd = self.ssthresh.saturating_add(self.mss);
                self.retransmit(now, stats);
            } else { self.cwnd = self.ssthresh; self.recovery = None; }
        } else if self.cwnd < self.ssthresh { self.cwnd = self.cwnd.saturating_add(acknowledged.min(self.mss)); }
        else {
            self.ca_bytes = self.ca_bytes.saturating_add(acknowledged);
            if self.ca_bytes >= self.cwnd {
                self.ca_bytes -= self.cwnd;
                self.cwnd = self.cwnd.saturating_add(self.mss);
            }
        }
    }

    fn receive_data(&mut self, now: u64, s: &Segment<'_>, stats: &mut Stats) {
        if self.peer_fin { stats.tcp_after_fin += 1; self.ack_now = true; return; }
        let start = self.rcv_nxt;
        let window = u32::from(self.window());
        let mut new_bytes = 0;
        let mut outside = false;
        for (i, &byte) in s.payload.iter().enumerate() {
            let seq = s.seq.wrapping_add(i as u32);
            if before(seq, start) { stats.tcp_duplicate_bytes += 1; continue; }
            if seq.wrapping_sub(start) >= window { outside = true; continue; }
            let slot = (seq & 65535) as usize;
            if self.present[slot] { stats.tcp_duplicate_bytes += 1; }
            else {
                self.received[slot] = byte;
                self.present[slot] = true;
                self.used += 1;
                new_bytes += 1;
            }
        }
        if outside { stats.tcp_out_of_window += 1; }
        if s.flags & FIN != 0 {
            let seq = s.seq.wrapping_add(s.payload.len() as u32);
            if seq.wrapping_sub(start) < window { self.peer_fin_seq.get_or_insert(seq); }
            else { stats.tcp_out_of_window += 1; }
        }
        if before(start, s.seq) && new_bytes != 0 { stats.tcp_out_of_order += 1; }
        let had_gap = self.used > new_bytes;
        let mut data = Vec::new();
        while self.present[(self.rcv_nxt & 65535) as usize] && self.peer_fin_seq != Some(self.rcv_nxt) {
            let slot = (self.rcv_nxt & 65535) as usize;
            data.push(self.received[slot]);
            self.present[slot] = false;
            self.used -= 1;
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
        }
        if !data.is_empty() {
            for event in self.ws.receive(&data, stats) {
                match event {
                    websocket::Event::Open(path) => {
                        self.opened = true;
                        self.events.push(HostEvent::Open { conn: self.id, endpoint: self.endpoint, path });
                    }
                    websocket::Event::Message(msg) => {
                        self.host_credit = self.host_credit.saturating_add(msg.len().max(1));
                        self.events.push(HostEvent::Message { conn: self.id, msg });
                    }
                    websocket::Event::Closed(code, reason) => self.closed(code, reason, stats),
                    websocket::Event::Send(bytes) => self.queue.extend(bytes),
                    websocket::Event::Finish => self.fin_wanted = true,
                }
            }
        }
        if self.peer_fin_seq == Some(self.rcv_nxt) {
            self.rcv_nxt = self.rcv_nxt.wrapping_add(1);
            self.peer_fin = true;
            self.fin_wanted = true;
            if let Some(response) = self.ws.eof(stats) { self.queue.extend(response); }
            self.closed(1006, "TCP EOF without WebSocket close".into(), stats);
            if self.used != 0 {
                stats.tcp_after_fin += 1;
                self.present.fill(false);
                self.used = 0;
            }
            self.ack_now = true;
        }
        self.ack_count = self.ack_count.saturating_add(1);
        if self.ack_count >= 2 || s.seq != start || had_gap || new_bytes == 0 || self.window() == 0 { self.ack_now = true; }
        self.ack_at.get_or_insert(now.saturating_add(40));
    }

    fn finish(&mut self, now: u64) {
        if self.peer_fin && self.fin_acked {
            if self.passive_close { self.done = true; }
            else if self.time_wait.is_none() { self.time_wait = Some(now.saturating_add(1000)); }
        }
    }

    pub fn tick(&mut self, now: u64, stats: &mut Stats) {
        if self.done { return; }
        if self.time_wait.is_some_and(|at| at <= now) { self.done = true; return; }
        if self.time_wait.is_some() {
            if self.ack_now { self.emit(self.snd_nxt, ACK, &[]); }
            return;
        }
        if self.retransmit_at.is_some_and(|at| at <= now) && (!self.established || self.peer_window != 0) {
            self.ssthresh = (self.snd_nxt.wrapping_sub(self.snd_una) / 2).max(2 * self.mss);
            self.cwnd = self.mss;
            self.ca_bytes = 0;
            self.recovery = None;
            self.duplicate_acks = 0;
            self.rto = self.rto.saturating_mul(2).min(self.rto_min.max(60_000));
            self.retransmit(now, stats);
        }
        if self.established {
            while !self.queue.is_empty() {
                let flight = self.snd_nxt.wrapping_sub(self.snd_una);
                let available = self.peer_window.min(self.cwnd).saturating_sub(flight);
                let count = (available.min(self.mss) as usize).min(self.queue.len());
                if count == 0 { break; }
                let data = self.queue.drain(..count).collect();
                self.transmit(now, ACK | 8, data);
            }
            if self.fin_wanted && !self.fin_sent && self.queue.is_empty()
                && self.snd_nxt.wrapping_sub(self.snd_una) < self.peer_window.min(self.cwnd) {
                self.passive_close = self.peer_fin;
                self.transmit(now, FIN | ACK, Vec::new());
                self.fin_sent = true;
            }
            if self.peer_window == 0 && (!self.queue.is_empty() || !self.flight.is_empty() || (self.fin_wanted && !self.fin_sent)) {
                self.retransmit_at = None;
                self.probe_at.get_or_insert(now.saturating_add(self.probe_interval));
                if self.probe_at.is_some_and(|at| at <= now) {
                    if let Some(flight) = self.flight.front_mut() {
                        flight.retransmitted = true;
                        let seq = flight.seq;
                        let data: Vec<u8> = flight.data.first().copied().into_iter().collect();
                        self.emit(seq, ACK, &data);
                    } else if let Some(&byte) = self.queue.front() {
                        // Account for this byte if the receiver's window reopened
                        // but its window-update ACK was lost.
                        self.queue.pop_front();
                        self.transmit(now, ACK, vec![byte]);
                        if let Some(flight) = self.flight.back_mut() { flight.retransmitted = true; }
                    } else { self.emit(self.snd_nxt.wrapping_sub(1), ACK, &[]); }
                    stats.tcp_probes += 1;
                    self.retransmit_at = None;
                    self.probe_interval = self.probe_interval.saturating_mul(2).min(self.rto_min.max(60_000));
                    self.probe_at = Some(now.saturating_add(self.probe_interval));
                }
            } else {
                self.probe_at = None;
                self.probe_interval = self.rto_min;
                if !self.flight.is_empty() { self.retransmit_at.get_or_insert(now.saturating_add(self.rto)); }
            }
        }
        if self.ack_now || self.ack_at.is_some_and(|at| at <= now) { self.emit(self.snd_nxt, ACK, &[]); }
        self.finish(now);
    }
}

//! Independent active TCP peer: cumulative ACKs, segment reassembly, a fixed
//! retransmit timer and a persist timer for a zero window. It shares only
//! checksum arithmetic with the server.
use std::collections::VecDeque;
use fab_elec_mcu_netgw::Message;
use super::wire::{read16, read32, tcp, ws, PORT};

fn earlier(a: u32, b: u32) -> bool { (a.wrapping_sub(b) as i32) < 0 }

struct Sent { seq: u32, flags: u8, data: Vec<u8>, at: u64 }

pub struct Client {
    pub port: u16,
    pub connected: bool,
    pub upgraded: bool,
    pub response: String,
    pub messages: Vec<Message>,
    pub pongs: Vec<Vec<u8>>,
    pub close: Option<(u16, String)>,
    pub peer_fin: bool,
    pub fin_acked: bool,
    pub window: u16,
    pub peer_window: u16,
    pub server_mss: u16,
    pub zero_windows: u64,
    pub retransmits: u64,
    next: u32,
    una: u32,
    expected: u32,
    wl_seq: u32,
    wl_ack: u32,
    sent: VecDeque<Sent>,
    queued: VecDeque<u8>,
    reordered: Vec<(u32, Vec<u8>, bool)>,
    wire: VecDeque<u8>,
    ack: bool,
    closing: bool,
    fin_sent: bool,
    want_fin: bool,
    probe_at: Option<u64>,
}

impl Client {
    pub fn new(port: u16) -> Self {
        // Cross the sequence wrap in the first large message.
        let isn = 0xffff_f000;
        Self {
            port, connected: false, upgraded: false, response: String::new(), messages: Vec::new(),
            pongs: Vec::new(), close: None, peer_fin: false, fin_acked: false,
            window: 65535, peer_window: 65535, server_mss: 536, zero_windows: 0,
            retransmits: 0, next: isn, una: isn, expected: 0,
            wl_seq: 0, wl_ack: 0, sent: VecDeque::new(), queued: VecDeque::new(),
            reordered: Vec::new(), wire: VecDeque::new(), ack: false, closing: false,
            fin_sent: false, want_fin: false, probe_at: None,
        }
    }

    pub fn queue(&mut self, bytes: Vec<u8>) { self.queued.extend(bytes); }

    pub fn close_device(&mut self) {
        self.queue(ws(8, true, true, &1000u16.to_be_bytes()));
        self.closing = true;
        self.want_fin = true;
    }

    pub fn advertise(&mut self, window: u16) { self.window = window; self.ack = true; }

    pub fn deadline(&self) -> Option<u64> { self.sent.front().map(|s| s.at + 250).into_iter().chain(self.probe_at).min() }

    pub fn idle(&self) -> bool { self.queued.is_empty() && self.sent.is_empty() }

    fn packet(&self, seq: u32, flags: u8, data: &[u8]) -> Vec<u8> {
        tcp(self.port, PORT, seq, self.expected, flags, self.window, data)
    }

    fn send(&mut self, now: u64, flags: u8, data: Vec<u8>, out: &mut Vec<Vec<u8>>) {
        out.push(self.packet(self.next, flags, &data));
        let sent = Sent { seq: self.next, flags, data, at: now };
        self.next = self.next.wrapping_add(sent.data.len() as u32 + u32::from(flags & 3 != 0));
        self.sent.push_back(sent);
    }

    pub fn poll(&mut self, now: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if !self.connected && self.sent.is_empty() { self.send(now, 2, Vec::new(), &mut out); }
        if self.sent.front().is_some_and(|s| s.at + 250 <= now) {
            let first = self.sent.front_mut().unwrap();
            first.at = now;
            let (seq, flags, data) = (first.seq, first.flags, first.data.clone());
            out.push(self.packet(seq, flags, &data));
            self.retransmits += 1;
        }
        if self.ack { out.push(self.packet(self.next, 16, &[])); self.ack = false; }
        if self.connected && self.peer_window == 0 && self.sent.is_empty() && !self.queued.is_empty() {
            // Persist timer (RFC 9293 3.8.6.1): a lost window update must not stall the sender.
            let at = *self.probe_at.get_or_insert(now + 250);
            if at <= now {
                out.push(self.packet(self.next.wrapping_sub(1), 16, &[]));
                self.probe_at = Some(now + 250);
            }
        } else { self.probe_at = None; }
        if self.connected {
            loop {
                let available = u32::from(self.peer_window).min(8192).saturating_sub(self.next.wrapping_sub(self.una));
                let count = self.queued.len().min(available as usize).min(usize::from(self.server_mss));
                if count == 0 { break; }
                let data = self.queued.drain(..count).collect();
                self.send(now, 24, data, &mut out);
            }
            if self.want_fin && !self.fin_sent && self.queued.is_empty() {
                self.send(now, 17, Vec::new(), &mut out);
                self.fin_sent = true;
            }
        }
        out
    }

    pub fn receive(&mut self, bytes: &[u8]) {
        if bytes[13] & 4 != 0 && self.peer_fin && self.fin_acked { return; }
        assert!(bytes[13] & 4 == 0, "accepted TCP connection never receives RST");
        let seq = read32(bytes, 4);
        let ack = read32(bytes, 8);
        let header = usize::from(bytes[12] >> 4) * 4;
        let data = &bytes[header..];
        if bytes[13] & 2 != 0 {
            assert_eq!(bytes[13] & 18, 18, "active open receives SYN ACK");
            assert_eq!(&bytes[20..22], &[2, 4], "SYN ACK offers only MSS");
            self.server_mss = read16(bytes, 22);
            if !self.connected {
                self.connected = true;
                self.expected = seq.wrapping_add(1);
                self.wl_seq = seq;
            }
            self.ack = true;
        }
        if !earlier(ack, self.una) && !earlier(self.next, ack) {
            self.una = ack;
            while self.sent.front().is_some_and(|s| !earlier(ack, s.seq.wrapping_add(s.data.len() as u32 + u32::from(s.flags & 3 != 0)))) {
                let sent = self.sent.pop_front().unwrap();
                if sent.flags & 1 != 0 { self.fin_acked = true; }
            }
            if let Some(sent) = self.sent.front_mut() {
                if earlier(sent.seq, ack) {
                    sent.data.drain(..ack.wrapping_sub(sent.seq) as usize);
                    sent.seq = ack;
                }
            }
            if !earlier(seq, self.wl_seq) && (seq != self.wl_seq || !earlier(ack, self.wl_ack)) {
                self.wl_seq = seq;
                self.wl_ack = ack;
                self.peer_window = read16(bytes, 14);
                if self.peer_window == 0 { self.zero_windows += 1; }
            }
        }
        if bytes[13] & 2 != 0 { return; }
        let fin = bytes[13] & 1 != 0;
        if data.is_empty() && !fin { return; }
        self.ack = true;
        assert!(data.len() <= 1200, "gateway respects client's MSS");
        if self.window == 0 { return; }
        self.reordered.push((seq, data.to_vec(), fin));
        loop {
            let Some(index) = self.reordered.iter().position(|(start, _, _)| !earlier(self.expected, *start)) else { break; };
            let (start, bytes, fin) = self.reordered.remove(index);
            let skip = self.expected.wrapping_sub(start) as usize;
            if skip < bytes.len() {
                self.wire.extend(&bytes[skip..]);
                self.expected = self.expected.wrapping_add((bytes.len() - skip) as u32);
            }
            if fin && start.wrapping_add(bytes.len() as u32) == self.expected {
                self.expected = self.expected.wrapping_add(1);
                self.peer_fin = true;
                self.want_fin = true;
            }
        }
        self.parse_ws();
    }

    fn parse_ws(&mut self) {
        if !self.upgraded {
            let data = self.wire.make_contiguous();
            let Some(end) = data.windows(4).position(|v| v == b"\r\n\r\n").map(|n| n + 4) else { return; };
            self.response = String::from_utf8(self.wire.drain(..end).collect()).unwrap();
            self.upgraded = self.response.starts_with("HTTP/1.1 101 ");
            if !self.upgraded { return; }
        }
        while self.wire.len() >= 2 {
            let kind = self.wire[0] & 15;
            assert_eq!(self.wire[0] & 128, 128, "server sends unfragmented messages");
            assert_eq!(self.wire[1] & 128, 0, "server frames are unmasked");
            let mut length = u64::from(self.wire[1]);
            let header = match length { 126 => 4, 127 => 10, _ => 2 };
            if self.wire.len() < header { break; }
            if header != 2 {
                length = 0;
                for i in 2..header { length = length * 256 + u64::from(self.wire[i]); }
            }
            if length > (self.wire.len() - header) as u64 { break; }
            self.wire.drain(..header);
            let data: Vec<u8> = self.wire.drain(..length as usize).collect();
            match kind {
                1 => self.messages.push(Message::Text(String::from_utf8(data).expect("server text is UTF-8"))),
                2 => self.messages.push(Message::Binary(data)),
                8 => {
                    let code = if data.len() >= 2 { read16(&data, 0) } else { 1005 };
                    let reason = if data.len() >= 2 { String::from_utf8(data[2..].to_vec()).unwrap() } else { String::new() };
                    self.close = Some((code, reason));
                    if !self.closing { self.queue(ws(8, true, true, &data)); self.closing = true; }
                    self.want_fin = true;
                }
                10 => self.pongs.push(data),
                _ => panic!("unexpected server WebSocket opcode {kind}"),
            }
        }
    }
}

pub mod wire;
mod client;

use fab_elec_mcu_netgw::{Config, Endpoint, Faults, Gateway, HostEvent, Link};
use fab_elec_mcu_rt::watch::{self, Event, Frame};
pub use client::Client;
use wire::*;

pub struct Device {
    pub link: Link,
    pub reader: watch::Reader,
    seq: u8,
    /// Tick of this device's latest Hello: the epoch every host packet must echo.
    hello_tick: Option<u32>,
    pub corrupt_next_eth: bool,
}

impl Device {
    pub fn new() -> Self {
        Self { link: Link::new(), reader: watch::Reader::new(), seq: 0, hello_tick: None, corrupt_next_eth: false }
    }

    fn receive(&mut self, packet: &[u8]) -> Vec<u8> {
        let mut decoded = None;
        for &byte in packet {
            if let Some(result) = self.reader.push(byte) {
                let frame = result.expect("host packet passes COBS and CRC checks");
                assert!(decoded.is_none(), "each host packet contains exactly one watch frame");
                assert_eq!(frame.kind, watch::ETH, "host sends only ETH packets");
                assert_eq!(Some(frame.tick), self.hello_tick, "host packets echo the tick of the device's latest Hello");
                let Event::Eth { len } = frame.event else { panic!("host packet decodes as ETH"); };
                let body = self.reader.eth_body();
                assert_eq!(body.len(), usize::from(len), "decoded ETH length matches the borrowed body");
                assert!((watch::ETH_MIN..=watch::ETH_MAX).contains(&body.len()), "ETH body has a valid Ethernet frame length");
                if read16(body, 12) == 0x0800 {
                    assert!(body.len() >= 34, "ETH body contains the full IPv4 header");
                    assert!(payload(body, body[23]).is_some(), "ETH body contains a valid IPv4 frame");
                }
                decoded = Some(body.to_vec());
            }
        }
        decoded.expect("host packet completes at its delimiter")
    }
}

pub struct Rig {
    pub gateway: Gateway,
    pub link: Option<Device>,
    pub now: u64,
    pub clients: Vec<Client>,
    pub events: Vec<HostEvent>,
    pub inbox: Vec<Vec<u8>>,
    pub echo: bool,
    pub drain_host: bool,
}

impl Rig {
    pub fn new(seed: Option<u64>) -> Self {
        let faults = seed.map_or_else(Faults::default, |seed| Faults {
            seed, drop_ppm: 50_000, dup_ppm: 20_000, reorder_ppm: 50_000, delay_max_ms: 20,
        });
        let config = Config {
            dns: vec![("bridge.invalid".into(), ENDPOINT_IP)],
            endpoints: vec![Endpoint { ip: ENDPOINT_IP, port: PORT, id: 7 }], faults,
            ..Config::default()
        };
        Self {
            gateway: Gateway::new(config), link: None, now: 0, clients: Vec::new(), events: Vec::new(), inbox: Vec::new(),
            echo: true, drain_host: true,
        }
    }

    pub fn frame_in(&mut self, frame: &[u8]) {
        if let Some(device) = &mut self.link {
            let mut wire = [0; watch::ETH_WIRE];
            let len = watch::encode_eth(device.seq, self.now as u32, frame, &mut wire).expect("device frame fits FC1");
            if std::mem::take(&mut device.corrupt_next_eth) {
                // The first COBS data byte is ETH's nonzero kind, not a block code.
                wire[1] ^= 1;
                assert_ne!(wire[1], 0, "CRC corruption must not introduce a delimiter");
                // Reuse this rejected packet's seq so later gaps measure only valid traffic.
            } else { device.seq = device.seq.wrapping_add(1); }
            for &byte in &wire[..len] { let _ = device.link.push(&mut self.gateway, self.now, byte); }
        } else { self.gateway.frame_in(self.now, frame); }
    }

    pub fn device_packet(&mut self, event: Event) {
        let kind = match event {
            Event::Hello { .. } => watch::HELLO,
            Event::Write { .. } => watch::WRITE,
            Event::Duty { .. } => watch::DUTY,
            Event::Read { .. } => watch::READ,
            Event::Fault { .. } => watch::FAULT,
            Event::Lost { .. } => watch::LOST,
            Event::Time { .. } => watch::TIME,
            Event::Probe { .. } => watch::PROBE,
            Event::Rx { .. } => watch::RX,
            Event::EthCredit { .. } => watch::ETH_CREDIT,
            Event::Link { .. } => watch::LINK,
            Event::RadioRecord { .. } => watch::RADIO_RECORD,
            Event::Shift { .. } => watch::SHIFT,
            Event::EepromRead { .. } => watch::EEPROM_READ,
            Event::EepromWrite { .. } => watch::EEPROM_WRITE,
            Event::Eth { .. } => panic!("send Ethernet frames with frame_in"),
            Event::Net { .. } => panic!("the gateway's devices send no network ports' messages"),
        };
        let device = self.link.as_mut().expect("device packet needs an attached device");
        let frame = Frame { kind, seq: device.seq, tick: self.now as u32, event };
        if kind == watch::HELLO { device.hello_tick = Some(frame.tick); }
        let mut wire = [0; watch::MAX_WIRE];
        let len = watch::encode(&frame, &mut wire);
        device.seq = device.seq.wrapping_add(1);
        for &byte in &wire[..len] { let _ = device.link.push(&mut self.gateway, self.now, byte); }
    }

    pub fn pump(&mut self) -> bool {
        let mut active = false;
        for index in 0..self.clients.len() {
            for frame in self.clients[index].poll(self.now) { self.frame_in(&frame); active = true; }
        }
        let frames = if let Some(device) = &mut self.link {
            let mut frames = Vec::new();
            device.link.poll(&mut self.gateway, self.now, &mut |frame| frames.push(frame.to_vec()));
            frames
        } else { self.gateway.frames_out(self.now) };
        for frame in frames {
            active = true;
            let frame = if let Some(device) = &mut self.link { device.receive(&frame) } else { frame };
            if let Some(tcp) = payload(&frame, 6) {
                if let Some(client) = self.clients.iter_mut().find(|c| c.port == read16(tcp, 2)) { client.receive(tcp); }
                else { self.inbox.push(frame); }
            } else { self.inbox.push(frame); }
            // A receive buffer is freed only after the device consumes its frame.
            if self.link.is_some() { self.device_packet(Event::EthCredit { count: 1 }); }
        }
        if self.drain_host {
            for event in self.gateway.host_events(self.now) {
                active = true;
                if self.echo {
                    if let HostEvent::Message { conn, ref msg } = event { self.gateway.host_send(self.now, conn, msg.clone()); }
                }
                self.events.push(event);
            }
        }
        active
    }

    fn step_time(&mut self, active: bool, limit: u64) {
        let deadline = self.gateway.next_deadline().into_iter().chain(self.clients.iter().filter_map(|c| c.deadline())).min();
        self.now = if active { self.now + 1 } else { deadline.unwrap_or(self.now + 1).max(self.now + 1) }.min(limit);
    }

    pub fn until(&mut self, property: &str, condition: impl Fn(&Self) -> bool) {
        let limit = self.now + 120_000;
        loop {
            let active = self.pump();
            if condition(self) { return; }
            assert!(self.now < limit, "{property}: virtual deadline reached; stats {:?}", self.gateway.stats());
            self.step_time(active, limit);
        }
    }

    pub fn for_ms(&mut self, millis: u64) {
        let limit = self.now + millis;
        while self.now < limit {
            let active = self.pump();
            self.step_time(active, limit);
        }
        self.pump();
    }

    pub fn exchange(&mut self, request: &[u8], matches: impl Fn(&[u8]) -> bool) -> Vec<u8> {
        let limit = self.now + 20_000;
        let mut retry = self.now;
        loop {
            if self.now >= retry { self.frame_in(request); retry = self.now + 250; }
            let active = self.pump();
            if let Some(index) = self.inbox.iter().position(|f| matches(f)) { return self.inbox.remove(index); }
            assert!(self.now < limit, "datagram exchange completes despite link faults");
            self.step_time(active, retry.min(limit));
        }
    }

    pub fn connect(&mut self) -> (usize, u64) {
        let index = self.clients.len();
        self.clients.push(Client::new(40000 + index as u16));
        self.until("three-way handshake", |r| r.clients[index].connected);
        self.clients[index].queue(upgrade());
        self.until("WebSocket upgrade reaches both peers", |r| r.clients[index].upgraded && r.events.iter().filter(|e| matches!(e, HostEvent::Open { .. })).count() > index);
        let conn = self.events.iter().filter_map(|event| if let HostEvent::Open { conn, endpoint, path } = event {
            assert_eq!(*endpoint, 7, "Open identifies configured host endpoint");
            assert_eq!(path, "/voice?stream=1", "Open preserves request path and query");
            Some(*conn)
        } else { None }).nth(index).unwrap();
        assert_eq!(self.clients[index].server_mss, 1460, "gateway advertises MTU minus IPv4/TCP headers");
        assert!(self.clients[index].response.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n"), "upgrade uses the RFC 6455 accept value");
        (index, conn)
    }
}

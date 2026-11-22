//! The tether host on link 0 of a booted S3. A test advances board time, then exchanges packets.
use fab_elec_mcu_emu::{board::Board, Machine};
use fab_elec_mcu_rt::{net::{Dir, Host, NetMsg}, watch};
use fab_elec_mcu_s3::S3Machine;

pub struct NetLink {
    pub host: Host,
    packet: Vec<u8>,
    pub hello: Option<watch::Frame>,
    pub seq: u8,
    pub up: Vec<(u32, NetMsg)>,
    pub down: Vec<NetMsg>,
    pub probes: Vec<(u32, u8, u16)>,
    pub faults: Vec<(u8, u16, i32)>,
}

impl NetLink {
    pub fn new(host: Host) -> Self {
        Self { host, packet: Vec::new(), hello: None, seq: 0, up: Vec::new(), down: Vec::new(),
            probes: Vec::new(), faults: Vec::new() }
    }

    pub fn send_down<B: Board>(&mut self, machine: &mut S3Machine<B>, msg: &NetMsg) {
        self.down.push(msg.clone());
        let mut wire = [0; watch::ETH_WIRE];
        let n = watch::encode_net(self.seq, self.hello.expect("Hello before host traffic").tick, msg, &mut wire);
        self.seq = self.seq.wrapping_add(1);
        for &b in &wire[..n] { machine.link_in(0, b); }
    }

    pub fn exchange<B: Board>(&mut self, machine: &mut S3Machine<B>, now_ns: u64) {
        self.read_up(machine, now_ns);
        if self.hello.is_some() {
            while let Some((_, msg)) = self.host.poll(now_ns) { self.send_down(machine, &msg); }
        }
    }

    pub fn read_up<B: Board>(&mut self, machine: &mut S3Machine<B>, now_ns: u64) {
        let mut out = Vec::new();
        machine.take_out(&mut out);
        for byte in out {
            assert_eq!(byte.link, 0, "the S3's tether uses link 0");
            if byte.byte != 0 { self.packet.push(byte.byte); continue; }
            if self.packet.is_empty() { continue; }
            let mut body = [0; watch::ETH_BODY];
            let n = watch::cobs_decode(&self.packet, &mut body).expect("device COBS");
            self.packet.clear();
            let body = &body[..n];
            if (watch::NET_OPEN..=watch::NET_LINK).contains(&body[0]) {
                assert!(n >= 7, "device NET frame has a header and CRC");
                assert_eq!(watch::crc8(&body[..n - 1]), body[n - 1], "device NET CRC");
                let msg = NetMsg::decode(body[0], Dir::Up, &body[6..n - 1]).expect("device NET payload and direction");
                assert!(self.hello.is_some(), "Open preceded by Hello");
                self.host.from_device(now_ns, &msg).unwrap_or_else(|e| panic!("host refused {msg:?}: {e:?}"));
                self.up.push((u32::from_le_bytes(body[2..6].try_into().unwrap()), msg));
            } else {
                let frame = watch::decode(body).expect("watch frame");
                match frame.event {
                    watch::Event::Hello { .. } => { self.hello = Some(frame); self.host.hello(now_ns); }
                    watch::Event::Probe { id, value } => self.probes.push((frame.tick, id, value)),
                    watch::Event::Fault { code, site, arg } => self.faults.push((code, site, arg)),
                    _ => {}
                }
            }
        }
    }
}

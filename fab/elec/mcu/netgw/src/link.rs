//! Host byte path for FC1 on a device's watch link.
//!
//! Drive a Link either with a Gateway via push/poll, or as a gateway-free
//! raw peer via push_raw/poll_raw. queue_frame serves both modes. Mixing
//! modes is allowed, but only push feeds a gateway; only poll collects its
//! output, and only a Hello through push resets it.
//! Device bytes pass through the watch Reader into Tether. Queued Ethernet output
//! passes through Tether and encode_eth. A queued radio restart uses the same
//! host sequence and goes out before Ethernet once the link has seen Hello.
//! Only Hello, ETH and ETH_CREDIT act on the tether. Every decoded result,
//! including errors and other watch events, is handed back to the caller.
//!
//! Host packets carry the tick of the latest device Hello (zero before any)
//! instead of a time: the epoch echo. The device takes an ETH only when it
//! echoes its own latest Hello, so a frame written before the host saw that
//! Hello (or held in an OS buffer across a reset) is dropped there and never
//! takes a receive buffer this lifetime's credit did not pay for.
//!
//! FC1 needs Config.mtu <= 1500. Larger gateway frames cannot be encoded
//! and are counted without sending or consuming a host sequence number.
//! Like the gateway and tether, the link is cloneable for simulator rewind.

use fab_elec_mcu_rt::watch::{self, Bad, Event, Frame};
use crate::{Gateway, Tether, TetherStats};
use crate::tether::FrameLength;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LinkStats {
    pub tether: TetherStats,
    pub unencodable: u64,
    /// Restart requests sent on the numbered host stream.
    pub radio_restarts_sent: u64,
    /// Queued restart requests discarded when a later Hello began a new device lifetime.
    pub radio_restarts_dropped_on_hello: u64,
}

/// One device's decoder, FC1 state and host packet encoder.
#[derive(Clone)]
pub struct Link {
    reader: watch::Reader,
    tether: Tether,
    seq: u8,
    /// The latest decoded Hello's tick, echoed in every host packet.
    epoch: u32,
    hello_seen: bool,
    restart_pending: bool,
    wire: [u8; watch::ETH_WIRE],
    unencodable: u64,
    radio_restarts_sent: u64,
    radio_restarts_dropped_on_hello: u64,
}

impl std::fmt::Debug for Link {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Link").field("tether", &self.tether).field("seq", &self.seq).field("epoch", &self.epoch)
            .field("hello_seen", &self.hello_seen).field("restart_pending", &self.restart_pending)
            .field("unencodable", &self.unencodable).field("radio_restarts_sent", &self.radio_restarts_sent)
            .field("radio_restarts_dropped_on_hello", &self.radio_restarts_dropped_on_hello).finish_non_exhaustive()
    }
}

impl Link {
    pub fn new() -> Self {
        Self { reader: watch::Reader::new(), tether: Tether::new(), seq: 0, epoch: 0,
            hello_seen: false, restart_pending: false, wire: [0; watch::ETH_WIRE], unencodable: 0,
            radio_restarts_sent: 0, radio_restarts_dropped_on_hello: 0 }
    }

    pub fn push(&mut self, gw: &mut Gateway, now_ms: u64, byte: u8) -> Option<Result<Frame, Bad>> {
        self.push_with_gateway(Some(gw), now_ms, byte)
    }

    /// Decode device bytes and update the tether without a gateway. Every
    /// decoded result or error is returned; borrow ETH bytes with eth_body()
    /// before the next push. Hello starts a new tether lifetime and epoch.
    /// Decoded ETH frames update tether statistics; malformed wire packets
    /// remain Reader errors, as with push.
    pub fn push_raw(&mut self, now_ms: u64, byte: u8) -> Option<Result<Frame, Bad>> {
        self.push_with_gateway(None, now_ms, byte)
    }

    fn push_with_gateway(&mut self, gw: Option<&mut Gateway>, now_ms: u64, byte: u8) -> Option<Result<Frame, Bad>> {
        let result = self.reader.push(byte);
        if let Some(Ok(frame)) = result {
            match frame.event {
                Event::Hello { .. } => {
                    if self.hello_seen && self.restart_pending {
                        self.restart_pending = false;
                        self.radio_restarts_dropped_on_hello += 1;
                    }
                    self.hello_seen = true;
                    self.epoch = frame.tick;
                    self.tether.hello_with_gateway(gw, now_ms);
                }
                Event::Eth { .. } => self.tether.eth_in_with_gateway(gw, now_ms, self.reader.eth_body()),
                Event::EthCredit { count } => self.tether.credit(now_ms, count),
                Event::Shift { .. } | Event::EepromRead { .. } | Event::EepromWrite { .. } => {}
                _ => {}
            }
        }
        result
    }

    pub fn eth_body(&self) -> &[u8] { self.reader.eth_body() }

    /// Copy an Ethernet II frame of 14..=1514 bytes into the tether's FIFO.
    /// Works in both modes: frames queued here and gateway output collected
    /// at poll keep FIFO order by the time they entered this shared queue.
    /// Sending waits for link-up and spends one credit per frame, using the
    /// current epoch echo and next host sequence number. Hello discards queued
    /// frames and counts them in frames_discarded_on_hello. An invalid length
    /// queues nothing and changes no statistics.
    pub fn queue_frame(&mut self, frame: &[u8]) -> Result<(), FrameLength> {
        self.tether.queue_frame(frame)
    }

    /// Queue one restart, coalescing repeated requests. It waits for the first Hello, then
    /// the next poll sends it before Ethernet with the current epoch echo and the next shared
    /// host sequence number. Its empty payload takes no receive buffer or credit. A later
    /// Hello discards and counts a pending request because the device has already restarted.
    pub fn queue_radio_restart(&mut self) {
        self.restart_pending = true;
    }

    pub fn poll(&mut self, gw: &mut Gateway, now_ms: u64, send: &mut dyn FnMut(&[u8])) {
        self.poll_with_gateway(Some(gw), now_ms, send);
    }

    /// Send a pending restart after Hello, then queued Ethernet frames allowed by credit.
    /// Gateway output stays queued at the gateway.
    /// Each callback must accept the whole encoded packet; it has no failure return.
    pub fn poll_raw(&mut self, now_ms: u64, send: &mut dyn FnMut(&[u8])) {
        self.poll_with_gateway(None, now_ms, send);
    }

    fn poll_with_gateway(&mut self, gw: Option<&mut Gateway>, now_ms: u64, send: &mut dyn FnMut(&[u8])) {
        if self.hello_seen && self.restart_pending {
            let mut wire = [0; watch::MAX_WIRE];
            let len = watch::encode_radio_restart(self.seq, self.epoch, &mut wire);
            send(&wire[..len]);
            self.seq = self.seq.wrapping_add(1);
            self.restart_pending = false;
            self.radio_restarts_sent += 1;
        }
        let Self { tether, seq, epoch, wire, unencodable, .. } = self;
        tether.poll_with_gateway(gw, now_ms, &mut |frame| {
            if let Some(len) = watch::encode_eth(*seq, *epoch, frame, wire) {
                send(&wire[..len]);
                *seq = seq.wrapping_add(1);
            } else { *unencodable += 1; }
        });
    }

    pub fn reader(&self) -> &watch::Reader { &self.reader }

    pub fn stats(&self) -> LinkStats {
        LinkStats { tether: self.tether.stats(), unencodable: self.unencodable,
            radio_restarts_sent: self.radio_restarts_sent, radio_restarts_dropped_on_hello: self.radio_restarts_dropped_on_hello }
    }
}

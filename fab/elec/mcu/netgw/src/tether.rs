//! Host side of FC1, the Ethernet frame channel on a device's watch link.
//!
//! ETH (kind 9) carries one unchanged Ethernet II frame, 14..=1514 bytes,
//! in either direction. ETH_CREDIT (kind 10) travels only from device to
//! host; its u16 count returns that many freed device receive buffers. The
//! host spends one credit per outgoing ETH frame. The wire codec belongs to
//! the caller: this module consumes decoded frames and credit counts.
//!
//! A device Hello starts a new link lifetime: forget all credits, mark the
//! link down, discard queued frames and reset a supplied gateway's stale sessions.
//! The first ETH_CREDIT, even a zero grant, marks the link up. Until then,
//! queued output waits in FIFO order; it still needs credit before sending.
//!
//! There is deliberately no queue cap. Gateway traffic is bounded by its
//! TCP windows (64 KiB per connection) and by DHCP/DNS being request/response.
//! Raw peers pace their own output using the queue length and credit stats.
//! An arbitrary cap here would turn back-pressure into random frame drops.
//! Like the gateway, the tether is cloneable for simulator rewind.

use std::collections::VecDeque;
use crate::Gateway;

/// A frame refused by `Link::queue_frame` for falling outside FC1's 14..=1514 bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameLength {
    pub len: usize,
}

impl FrameLength {
    fn check(len: usize) -> Result<(), Self> {
        if (14..=1514).contains(&len) { Ok(()) } else { Err(Self { len }) }
    }
}

impl std::fmt::Display for FrameLength {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Ethernet frame length {} is outside 14..=1514 bytes", self.len)
    }
}

impl std::error::Error for FrameLength {}

/// Lifetime counters and current link state. Hello does not clear counters
/// or the queue high-water mark.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TetherStats {
    /// All device ETH frames, including those rejected for their length.
    pub frames_in: u64,
    pub frames_sent: u64,
    pub bad_lengths: u64,
    pub hellos: u64,
    /// Sum of granted buffer counts, not the number of ETH_CREDIT messages.
    pub credits_received: u64,
    /// Grants whose resulting outstanding total exceeds u16::MAX, the
    /// largest receive-buffer pool representable by FC1's credit count.
    /// Excess credit is retained, saturating only at u32::MAX.
    pub credit_anomalies: u64,
    pub frames_discarded_on_hello: u64,
    pub queue_len: usize,
    /// Maximum queued frames, sampled on enqueue and before spending credits in poll.
    pub queue_high_water: usize,
    pub credits: u32,
    pub link_up: bool,
}

/// One device's FC1 state, independent of gateway ownership and wire I/O.
/// The FIFO has no cap; see the module's flow-control contract.
#[derive(Clone, Debug, Default)]
pub struct Tether {
    queue: VecDeque<Vec<u8>>,
    stats: TetherStats,
}

impl Tether {
    /// Start down, with no credits and no queued frames.
    pub fn new() -> Self { Self::default() }

    /// Start a fresh device lifetime and close its old gateway connections.
    pub fn hello(&mut self, gw: &mut Gateway, now_ms: u64) {
        self.hello_with_gateway(Some(gw), now_ms);
    }

    pub(crate) fn hello_with_gateway(&mut self, gw: Option<&mut Gateway>, now_ms: u64) {
        self.stats.hellos += 1;
        self.stats.credits = 0;
        self.stats.link_up = false;
        self.stats.frames_discarded_on_hello += self.queue.len() as u64;
        self.queue.clear();
        if let Some(gw) = gw { gw.device_reset(now_ms); }
    }

    /// Forward a decoded device ETH frame only if its FC1 length is valid.
    pub fn eth_in(&mut self, gw: &mut Gateway, now_ms: u64, frame: &[u8]) {
        self.eth_in_with_gateway(Some(gw), now_ms, frame);
    }

    pub(crate) fn eth_in_with_gateway(&mut self, gw: Option<&mut Gateway>, now_ms: u64, frame: &[u8]) {
        self.stats.frames_in += 1;
        if FrameLength::check(frame.len()).is_err() {
            self.stats.bad_lengths += 1;
            return;
        }
        if let Some(gw) = gw { gw.frame_in(now_ms, frame); }
    }

    pub(crate) fn queue_frame(&mut self, frame: &[u8]) -> Result<(), FrameLength> {
        FrameLength::check(frame.len())?;
        self.queue.push_back(frame.to_vec());
        self.stats.queue_high_water = self.stats.queue_high_water.max(self.queue.len());
        Ok(())
    }

    /// Return receive buffers and mark the link up. There is no credit timer;
    /// now_ms follows the gateway's virtual-clock interface for the caller.
    pub fn credit(&mut self, _now_ms: u64, count: u16) {
        self.stats.credits_received += u64::from(count);
        self.stats.credits = self.stats.credits.saturating_add(u32::from(count));
        if self.stats.credits > u32::from(u16::MAX) {
            self.stats.credit_anomalies += 1;
        }
        self.stats.link_up = true;
    }

    /// Collect gateway output, then send the oldest frames allowed by credit.
    /// Each callback must accept the whole frame; it has no failure return.
    pub fn poll(&mut self, gw: &mut Gateway, now_ms: u64, send: &mut dyn FnMut(&[u8])) {
        self.poll_with_gateway(Some(gw), now_ms, send);
    }

    pub(crate) fn poll_with_gateway(&mut self, gw: Option<&mut Gateway>, now_ms: u64, send: &mut dyn FnMut(&[u8])) {
        if let Some(gw) = gw { self.queue.extend(gw.frames_out(now_ms)); }
        self.stats.queue_high_water = self.stats.queue_high_water.max(self.queue.len());
        while self.stats.link_up && self.stats.credits > 0 {
            let Some(frame) = self.queue.pop_front() else { break; };
            send(&frame);
            self.stats.credits -= 1;
            self.stats.frames_sent += 1;
        }
    }

    pub fn stats(&self) -> TetherStats {
        TetherStats { queue_len: self.queue.len(), ..self.stats }
    }
}

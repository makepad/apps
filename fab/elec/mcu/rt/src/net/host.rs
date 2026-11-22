//! `Host`: the host end of a device's link, over a [`World`]. It speaks [`NetMsg`] with the device's
//! ports layer, whether the messages travel as FC1 frames on the tether or straight from the
//! reference interpreter, and keeps the flow control both ways:
//!
//! - down: a part of `c` goes only against a receive slot the device has free. Slots start at `c`'s
//!   declared count when `c` goes ready; the device's `Freed` returns them.
//! - up: the device sends only within bytes granted: [`super::FIRST_GRANT`] at ready, then top-ups of
//!   at least [`super::GRANT_STEP`] as the world's upstream drains (its room less what is granted and
//!   not yet sent), so the host never holds more than the world's window.
//! - every OPEN gets exactly one offline back: its failure, its loss, or the answer to its CLOSE. A
//!   CLOSE for a connection that already ended gets none (that end crossed it).
//! - a device Hello ends every connection and forgets every credit.

use super::upstream::{Arrival, Request};
use super::{code, NetMsg, Payload, World, WorldEvent, FIRST_GRANT, GRANT_STEP, NET_PART, OFFLINE, PART_BINARY, PART_FIRST, PART_LAST, READY};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// A device message the host end refuses: a ports-layer bug, never a race.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostError {
    /// No connection `c` is declared.
    Unknown { c: u8 },
    /// A message only the host sends.
    Direction,
    /// An OPEN for a connection the host already holds open.
    Reopen { c: u8 },
    /// Data beyond the bytes granted.
    OverGrant { c: u8 },
    /// A part that starts a message inside another, or continues none.
    Order { c: u8 },
    /// More slots freed than the connection declares.
    OverFree { c: u8 },
}

#[derive(Clone, Debug, Default)]
struct Conn {
    declared: u16,
    /// An OPEN came and its one offline has not gone back.
    live: bool,
    ready: bool,
    slots: u16,
    /// Whole messages for the device, shared with the world's: (binary, bytes, bytes already sent).
    /// A part is cut only as a slot takes it.
    down: VecDeque<(bool, Payload, usize)>,
    granted: u64,
    received: u64,
    /// A device message is between its first and last parts.
    up_open: bool,
}

#[derive(Clone, Debug)]
pub struct Host {
    world: World,
    conns: Vec<Conn>,
    out: VecDeque<(u64, NetMsg)>,
    now: u64,
    /// Device data for connections already ended (crossed in flight), dropped.
    pub stale: u64,
}

impl Host {
    /// The host end for a device whose connection `c` declares `slots[c]` receive slots.
    pub fn new(world: World, slots: &[u16]) -> Host {
        let conns = slots.iter().map(|&declared| Conn { declared, ..Conn::default() }).collect();
        Host { world, conns, out: VecDeque::new(), now: 0, stale: 0 }
    }

    pub fn world(&self) -> &World { &self.world }

    /// The next request for the real network, stamped no earlier than the host's time, so requests
    /// come out in time order. Take them after every call.
    pub fn request(&mut self) -> Option<(u64, Request)> {
        self.world.request().map(|(t, request)| (t.max(self.now), request))
    }

    /// An answer from the real network at `t` (time order with the other calls); [`Host::poll`]
    /// then carries what it changed to the device.
    pub fn arrive(&mut self, t: u64, arrival: Arrival) {
        self.now = self.now.max(t);
        self.world.arrive(self.now, arrival);
    }

    /// The network under every upstream connection was closed at `t`: each reads as reset.
    pub fn cut_upstream(&mut self, t: u64) {
        self.now = self.now.max(t);
        self.world.cut_upstream(self.now);
    }

    /// The device (re)started at `t`: every connection ends without a word, every credit is
    /// forgotten, and the device hears the link as it stands.
    pub fn hello(&mut self, t: u64) {
        self.now = self.now.max(t);
        for (c, conn) in self.conns.iter_mut().enumerate() {
            if conn.live { self.world.close(t, c as u8); }
            *conn = Conn { declared: conn.declared, ..Conn::default() };
        }
        self.out.clear();
        let (state, code, detail) = self.world.link();
        self.out.push_back((self.now, NetMsg::Link { state, code, detail }));
    }

    /// One message from the device at `t` (time order).
    pub fn from_device(&mut self, t: u64, msg: &NetMsg) -> Result<(), HostError> {
        self.now = self.now.max(t);
        let t = self.now;
        let c = msg.conn().ok_or(HostError::Direction)?;
        let conn = self.conns.get_mut(c as usize).ok_or(HostError::Unknown { c })?;
        match msg {
            NetMsg::Open { .. } => {
                if conn.live { return Err(HostError::Reopen { c }); }
                *conn = Conn { declared: conn.declared, live: true, ..Conn::default() };
                self.world.open(t, c);
            }
            NetMsg::Close { .. } => {
                if conn.live {
                    *conn = Conn { declared: conn.declared, ..Conn::default() };
                    self.world.close(t, c);
                    self.out.push_back((t, NetMsg::State { c, state: OFFLINE, code: code::NONE, detail: 0 }));
                }
            }
            NetMsg::Data { flags, bytes, .. } => {
                if !conn.ready {
                    self.stale += 1;
                    return Ok(());
                }
                if conn.received + bytes.len() as u64 > conn.granted { return Err(HostError::OverGrant { c }); }
                if (flags & PART_FIRST != 0) == conn.up_open { return Err(HostError::Order { c }); }
                conn.up_open = flags & PART_LAST == 0;
                conn.received += bytes.len() as u64;
                self.world.send(t, c, *flags, bytes);
                self.top_up(t, c);
            }
            NetMsg::Freed { slots, .. } => {
                if !conn.ready {
                    self.stale += 1;
                    return Ok(());
                }
                if u32::from(conn.slots) + u32::from(*slots) > u32::from(conn.declared) { return Err(HostError::OverFree { c }); }
                conn.slots += slots;
                self.flush(c);
            }
            NetMsg::Granted { .. } | NetMsg::State { .. } | NetMsg::Link { .. } => return Err(HostError::Direction),
        }
        Ok(())
    }

    /// When the host next has something for the device, or work of its own.
    pub fn next_due(&self) -> Option<u64> {
        let out = self.out.front().map(|(t, _)| *t);
        let grants = (0..self.conns.len()).filter_map(|c| self.grant_due(c as u8));
        [out, self.world.next_due()].into_iter().flatten().chain(grants).min().map(|t| t.max(self.now))
    }

    /// The next message for the device at or before `now`, in time order.
    pub fn poll(&mut self, now: u64) -> Option<(u64, NetMsg)> {
        loop {
            if let Some(&(t, _)) = self.out.front() {
                if t <= now { return self.out.pop_front(); }
            }
            // The earliest of the world's next step and a connection's next top-up, the world first
            // on a tie, so time only moves forward.
            let grant = (0..self.conns.len() as u8).filter_map(|c| self.grant_due(c).map(|t| (t, c))).min();
            let world = self.world.next_due();
            match (world, grant) {
                (Some(tw), g) if tw <= now && g.is_none_or(|(tg, _)| tw <= tg) => {
                    if let Some((t, event)) = self.world.poll(tw) {
                        self.now = self.now.max(t);
                        self.on_world(self.now, event);
                    }
                }
                (_, Some((tg, c))) if tg <= now => {
                    self.now = self.now.max(tg);
                    self.top_up(self.now, c);
                    debug_assert!(self.grant_due(c).is_none_or(|t| t > self.now), "a due top-up grants");
                }
                _ => return None,
            }
        }
    }

    fn on_world(&mut self, t: u64, event: WorldEvent) {
        match event {
            WorldEvent::Link { state, code, detail } => self.out.push_back((t, NetMsg::Link { state, code, detail })),
            WorldEvent::State { c, state, code, detail } => {
                let Some(conn) = self.conns.get_mut(c as usize).filter(|conn| conn.live) else { return };
                if state == READY {
                    conn.ready = true;
                    conn.slots = conn.declared;
                    conn.granted = u64::from(FIRST_GRANT);
                    conn.received = 0;
                    self.out.push_back((t, NetMsg::State { c, state, code, detail }));
                    self.out.push_back((t, NetMsg::Granted { c, bytes: FIRST_GRANT }));
                } else {
                    *conn = Conn { declared: conn.declared, ..Conn::default() };
                    self.out.push_back((t, NetMsg::State { c, state: OFFLINE, code, detail }));
                }
            }
            WorldEvent::Message { c, binary, bytes } => {
                // an empty message has no part to carry it
                let Some(conn) = self.conns.get_mut(c as usize).filter(|conn| conn.ready && !bytes.is_empty()) else { return };
                conn.down.push_back((binary, bytes, 0));
                self.flush(c);
            }
        }
    }

    fn flush(&mut self, c: u8) {
        let conn = &mut self.conns[c as usize];
        while conn.slots > 0 {
            let Some((binary, message, at)) = conn.down.front_mut() else { break };
            let end = (*at + NET_PART).min(message.len());
            let flags = if *binary { PART_BINARY } else { 0 } | if *at == 0 { PART_FIRST } else { 0 }
                | if end == message.len() { PART_LAST } else { 0 };
            let bytes = message[*at..end].to_vec();
            if end == message.len() { conn.down.pop_front(); } else { *at = end; }
            conn.slots -= 1;
            self.out.push_back((self.now, NetMsg::Data { c, flags, bytes }));
        }
    }

    /// The grant `c` may have now: the world's room less what is granted and not yet sent.
    fn grant_room(&self, t: u64, c: u8) -> u64 {
        let conn = &self.conns[c as usize];
        let outstanding = conn.granted - conn.received;
        u64::from(self.world.room(t, c)).saturating_sub(outstanding)
    }

    /// When `c`'s next top-up of at least GRANT_STEP becomes possible.
    fn grant_due(&self, c: u8) -> Option<u64> {
        let conn = &self.conns[c as usize];
        if !conn.ready { return None; }
        let outstanding = conn.granted - conn.received;
        self.world.room_at(self.now, c, (outstanding + u64::from(GRANT_STEP)).min(u64::from(u32::MAX)) as u32)
    }

    fn top_up(&mut self, t: u64, c: u8) {
        let room = self.grant_room(t, c);
        if room >= u64::from(GRANT_STEP) {
            self.conns[c as usize].granted += room;
            self.out.push_back((t, NetMsg::Granted { c, bytes: room as u32 }));
        }
    }
}

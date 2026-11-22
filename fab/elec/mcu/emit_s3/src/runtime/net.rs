//! The device end of the network ports. The tether carries messages over the watch link.
use super::{cc, ccz, Code, Link, LinkSram, SramLayout, EVENT, NET_FLUSH, PORT, TURN, WIRE};
use crate::image::NetSram;
use fab_elec_mcu_rt::{net, ports, watch};
mod data;

pub(crate) const SLOT_BYTES: u32 = (4 + net::NET_PART as u32 + 3) & !3;
pub(crate) const ROW_BYTES: u32 = 128;
const DUE: u32 = 0;
const RETRY: u32 = 8;
const PENDING: u32 = 16;
const RETRYING: u32 = 20;
const WANTED: u32 = 24;
const LIVE: u32 = 28;
const CLOSING: u32 = 32;
const STATE: u32 = 36;
const ERROR: u32 = 40;
const DETAIL: u32 = 44;
const EPOCH: u32 = 48;
const BACKOFF: u32 = 52;
const OPEN_SENT: u32 = 56;
const GRANT: u32 = 60; // little-endian u64
const RX_WRITE: u32 = 68;
const RX_USED: u32 = 72;
const RX_OPEN: u32 = 76;
const RX_TAIL: u32 = 80;
const RX_OFFSET: u32 = 84;
const RX_READING: u32 = 88; // 0 between messages, otherwise PART_FIRST | kind
const OWED: u32 = 92;
const TX_HEAD: u32 = 96;
const TX_TAIL: u32 = 100;
const TX_USED: u32 = 104;
const RESERVED: u32 = 108; // granted prefix in the outbound FIFO
const TX_OPEN: u32 = 112; // 0 between messages, otherwise PART_FIRST | kind
const WAIT_SPACE: u32 = 116;
// SYSTIMER runs at 16 MHz (the runtime stamp, facts-machine.md §1).
const FIRST: u32 = (net::RETRY_FIRST_NS * 16 / 1000) as u32;
const MAX_WAIT: u32 = (net::RETRY_MAX_NS * 16 / 1000) as u32;

// a8 is the connection row throughout. No network input runs inside an app
// handler: its first unacknowledged change always belongs to the next call.
fn put(c: &mut Code<'_>, offset: u32, value: u32) {
    c.imm(5, value); c.a.s32i(5, 8, offset);
}

fn mark(c: &mut Code<'_>, l: &SramLayout) {
    let done = c.label(); c.a.l32i(2, 8, PENDING); c.zero(ccz::NE, 2, done);
    for i in [0, 4] { c.load(2, l.stamp + i); c.a.s32i(2, 8, DUE + i); }
    put(c, PENDING, 1); c.bind(done);
}

fn clear_data(c: &mut Code<'_>) {
    c.imm(5, 0);
    for off in [GRANT, GRANT + 4, RX_WRITE, RX_USED, RX_OPEN, RX_TAIL, RX_OFFSET, RX_READING, TX_OPEN, WAIT_SPACE] {
        c.a.s32i(5, 8, off);
    }
    // A granted prefix and owed Freed already precede a Close on the wire.
    // The host answers the next Open only after those tickets have gone.
    c.a.l32i(2, 8, RESERVED); c.a.s32i(2, 8, TX_USED);
    let pending = c.label(); c.zero(ccz::NE, 2, pending);
    c.a.s32i(5, 8, TX_HEAD); c.a.s32i(5, 8, TX_TAIL); c.bind(pending);
}

fn end_ready(c: &mut Code<'_>) {
    let skip = c.label(); c.a.l32i(2, 8, STATE); c.imm(3, u32::from(net::READY));
    c.branch(cc::NE, 2, 3, skip); c.a.l32i(2, 8, EPOCH); c.a.addi(2, 2, 1); c.a.s32i(2, 8, EPOCH);
    c.bind(skip);
}

fn retry_after(c: &mut Code<'_>, l: &SramLayout) {
    c.load(2, l.stamp); c.load(3, l.stamp + 4); c.a.l32i(4, 8, BACKOFF);
    c.imm(5, 0); c.add64(2, 3, 4, 5, 6, 7);
    c.a.s32i(2, 8, RETRY); c.a.s32i(3, 8, RETRY + 4); put(c, RETRYING, 1);
    c.a.add_n(4, 4, 4); c.imm(5, MAX_WAIT); c.a.minu(4, 4, 5); c.a.s32i(4, 8, BACKOFF);
}

fn row(c: &mut Code<'_>, n: &NetSram, id: u8) {
    c.a.slli(8, id, ROW_BYTES.ilog2() as u8); c.address(11, n.rows); c.a.add_n(8, 8, 11);
}

fn ordinal(c: &mut Code<'_>, n: &NetSram) {
    c.address(2, n.rows); c.a.sub(2, 8, 2); c.shr(2, 2, ROW_BYTES.ilog2());
}

// One FIFO across all connections preserves the application's order under
// backpressure. An unsent Open cannot be answered. It can have a Close behind
// it and one crossed Close ahead of it (the previous Open's offline arrived
// first). A fourth command needs a new host answer to a transmitted Open.
fn advance_ticket(c: &mut Code<'_>, n: &NetSram, cursor: u32) {
    c.a.addi(3, 3, 1); c.imm(4, n.ticket_count);
    let fits = c.label(); c.branch(cc::LTU, 3, 4, fits); c.imm(3, 0); c.bind(fits); c.store(3, cursor);
}

// Low byte: connection. Next two bits: Open, Close, Data or Freed, in FC1
// kind order. Remaining bits: length of the reserved Data run (bounded by SRAM).
fn queue(c: &mut Code<'_>, n: &NetSram, kind: u8) {
    ordinal(c, n); c.imm(4, u32::from(kind - watch::NET_OPEN) << 8); c.a.or(2, 2, 4);
    if kind == watch::NET_DATA {
        // a6 holds the newly granted length. Only adjacent Data for this
        // connection may merge; preserve intervening credits and controls.
        c.a.slli(6, 6, 10);
        let append = c.label(); let merged = c.label();
        c.load(3, n.ticket_head); c.load(4, n.ticket_tail); c.branch(cc::EQ, 3, 4, append);
        let previous = c.label(); c.zero(ccz::NE, 3, previous); c.imm(3, n.ticket_count);
        c.bind(previous); c.a.addi(3, 3, -1); c.address(4, n.tickets); c.a.addx4(4, 3, 4);
        c.a.l32i(3, 4, 0); c.a.extui(5, 3, 0, 10); c.branch(cc::NE, 2, 5, append);
        c.a.add_n(3, 3, 6); c.a.s32i(3, 4, 0); c.jump(merged);
        c.bind(append); c.a.or(2, 2, 6); append_ticket(c, n); c.bind(merged);
    } else { append_ticket(c, n); }
}

fn append_ticket(c: &mut Code<'_>, n: &NetSram) {
    c.load(3, n.ticket_head); c.address(4, n.tickets); c.a.addx4(4, 3, 4);
    c.a.s32i(2, 4, 0); advance_ticket(c, n, n.ticket_head);
}

// a4 ticket address, a6 reserved length, a7 FC1 kind, a8 connection row.
fn front(c: &mut Code<'_>, n: &NetSram) {
    c.load(3, n.ticket_tail); c.address(4, n.tickets); c.a.addx4(4, 3, 4); c.a.l32i(2, 4, 0);
    c.a.extui(7, 2, 8, 2); c.a.addi(7, 7, i32::from(watch::NET_OPEN));
    c.a.srli(6, 2, 10); c.a.extui(2, 2, 0, 8); row(c, n, 2);
}

pub(super) fn flush(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, e: &LinkSram, link: &Link) {
    c.routine(NET_FLUSH, "net_flush"); c.store(0, n.flush_return);
    let body = n.tx.as_ref().map_or(l.body, |tx| tx.body);
    let again = c.label(); let done = c.label();
    tether::writable(c, l, n, e, done);
    c.bind(again); c.load(3, n.ticket_tail); c.load(4, n.ticket_head); c.branch(cc::EQ, 3, 4, done);
    front(c, n); c.byte(2, body + 6); c.byte(7, body);
    let send = c.label(); let control = c.label();
    if let Some(tx) = &n.tx {
        let credit = c.label(); c.imm(3, u32::from(watch::NET_DATA)); c.branch(cc::NE, 7, 3, credit);
        data::part(c, n, tx, link); c.jump(send); c.bind(credit);
    }
    c.imm(3, u32::from(watch::NET_CREDIT)); c.branch(cc::NE, 7, 3, control);
    c.a.l32i(2, 8, OWED); c.byte(2, body + 7); c.a.srli(2, 2, 8); c.byte(2, body + 8);
    c.imm(2, 3); c.jump(send); c.bind(control); c.imm(2, 1);
    c.bind(send); c.address(3, body); tether::send(c, l); c.zero(ccz::EQ, 2, done);
    front(c, n);
    let pop = c.label(); let freed = c.label();
    if let Some(tx) = &n.tx {
        let other = c.label(); c.imm(3, u32::from(watch::NET_DATA)); c.branch(cc::NE, 7, 3, other);
        data::sent(c, l, n, tx, link, again); c.jump(pop); c.bind(other);
    }
    c.imm(3, u32::from(watch::NET_OPEN)); c.branch(cc::NE, 7, 3, freed);
    put(c, OPEN_SENT, 1); c.jump(pop);
    c.bind(freed); c.imm(3, u32::from(watch::NET_CREDIT)); c.branch(cc::NE, 7, 3, pop); put(c, OWED, 0);
    c.bind(pop); c.load(3, n.ticket_tail); advance_ticket(c, n, n.ticket_tail); c.jump(again);
    c.bind(done); c.load(0, n.flush_return); c.a.ret_n();
}

pub(super) fn port(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, link: &Link, port: u16) {
    c.routine(PORT + usize::from(port), ports::PORTS[usize::from(port)].name);
    if port <= ports::NET_LINK_DETAIL {
        c.load(2, n.link_state + 4 * u32::from(port - ports::NET_LINK)); c.a.ret_n(); return;
    }
    let valid = c.label(); c.imm(7, n.connections.len() as u32); c.branch(cc::LTU, 2, 7, valid);
    c.store(0, l.port_return); c.a.mov_n(5, 2); c.a.mov_n(4, 10);
    c.imm(3, u32::from(watch::FAULT_NET_HANDLE)); c.imm(2, u32::from(watch::FAULT)); c.call(EVENT);
    c.load(0, l.port_return); c.imm(2, 0); c.a.ret_n();
    c.bind(valid); row(c, n, 2);
    match port {
        ports::NET_OPEN | ports::NET_CLOSE => {
            let unchanged = c.label(); let changed = c.label();
            c.a.l32i(2, 8, WANTED);
            if port == ports::NET_OPEN {
                c.zero(ccz::NE, 2, unchanged); put(c, WANTED, 1); put(c, RETRYING, 0);
                put(c, BACKOFF, FIRST); put(c, STATE, u32::from(net::CONNECTING));
                c.a.l32i(2, 8, CLOSING); c.zero(ccz::NE, 2, changed);
                put(c, LIVE, 1); queue(c, n, watch::NET_OPEN);
            } else {
                c.zero(ccz::EQ, 2, unchanged); put(c, WANTED, 0); put(c, RETRYING, 0);
                end_ready(c); put(c, STATE, u32::from(net::OFFLINE)); put(c, ERROR, 0); put(c, DETAIL, 0); clear_data(c);
                c.a.l32i(2, 8, LIVE); c.zero(ccz::EQ, 2, changed);
                put(c, LIVE, 0); put(c, CLOSING, 1); queue(c, n, watch::NET_CLOSE);
            }
            c.bind(changed); c.store(0, l.port_return); c.call(NET_FLUSH); c.load(0, l.port_return); c.imm(2, 1); c.a.ret_n();
            c.bind(unchanged); c.imm(2, 0);
        }
        ports::NET_STATE => c.a.l32i(2, 8, STATE),
        ports::NET_ERROR => c.a.l32i(2, 8, ERROR),
        ports::NET_DETAIL => c.a.l32i(2, 8, DETAIL),
        ports::NET_EPOCH => c.a.l32i(2, 8, EPOCH),
        ports::NET_LEFT | ports::NET_KIND => data::inspect(c, n, link, port),
        ports::NET_SEND | ports::NET_RECV => data::transfer(c, l, n, link, port),
        _ => unreachable!("connection port"),
    }
    c.a.ret_n();
}

fn rows(c: &mut Code<'_>, n: &NetSram, body: impl FnOnce(&mut Code<'_>)) {
    c.address(8, n.rows); let again = c.label(); c.bind(again); c.feed();
    body(c);
    c.imm(5, ROW_BYTES); c.a.add_n(8, 8, 5);
    c.address(5, n.rows + ROW_BYTES * n.connections.len() as u32); c.branch(cc::LTU, 8, 5, again);
}

pub(super) fn hello(c: &mut Code<'_>, l: &SramLayout, n: &NetSram) {
    // Host::hello forgets all opens and grants. At boot WANTED is zero.
    // On a tether reconnect, expose the end of the old lifetime, then retry
    // the connections the application still wants after their first backoff.
    c.set(n.ticket_head, 0); c.set(n.ticket_tail, 0);
    if let Some(tx) = &n.tx { c.set(tx.part_bytes, 0); }
    c.load(2, n.link_state); c.load(3, n.link_state + 4); c.a.or(2, 2, 3);
    c.load(3, n.link_state + 8); c.a.or(2, 2, 3);
    let unchanged = c.label(); c.zero(ccz::EQ, 2, unchanged);
    for off in [0, 4, 8] { c.set(n.link_state + off, 0); }
    rows(c, n, |c| mark(c, l)); c.bind(unchanged);
    rows(c, n, |c| {
        end_ready(c); put(c, RESERVED, 0); put(c, OWED, 0); clear_data(c);
        put(c, OPEN_SENT, 0); put(c, CLOSING, 0); put(c, LIVE, 0);
        put(c, STATE, u32::from(net::OFFLINE)); put(c, RETRYING, 0); put(c, BACKOFF, FIRST);
        let idle = c.label(); c.a.l32i(2, 8, WANTED); c.zero(ccz::EQ, 2, idle);
        put(c, ERROR, u32::from(net::code::LINK_LOST)); put(c, DETAIL, 0); retry_after(c, l); mark(c, l);
        c.bind(idle);
    });
}

// Timers and Ethernet have already competed. Strictly earlier wins. Within
// one connection retry comes before its handler on a tie (rt::sched::next_event).
pub(super) fn scan(c: &mut Code<'_>, n: &NetSram) {
    rows(c, n, |c| {
        for (armed, at, tag) in [(RETRYING, RETRY, 1), (PENDING, DUE, 0)] {
            let skip = c.label(); let choose = c.label();
            c.a.l32i(2, 8, armed); c.zero(ccz::EQ, 2, skip);
            c.a.l32i(4, 8, at); c.a.l32i(5, 8, at + 4); c.zero(ccz::EQ, 13, choose);
            c.branch(cc::LTU, 5, 15, choose); c.branch(cc::NE, 5, 15, skip); c.branch(cc::GEU, 4, 14, skip);
            c.bind(choose); c.a.addi(13, 8, tag); c.a.mov_n(14, 4); c.a.mov_n(15, 5); c.bind(skip);
        }
    });
}

fn config(c: &mut Code<'_>, n: &NetSram, link: &Link) {
    ordinal(c, n); c.a.slli(2, 2, 5); c.address(7, link.net_config); c.a.add_n(7, 7, 2);
}

pub(super) fn select(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, link: &Link, enter: usize) {
    let other = c.label(); c.address(2, n.rows); c.branch(cc::LTU, 13, 2, other);
    c.address(2, n.rows + ROW_BYTES * n.connections.len() as u32); c.branch(cc::GEU, 13, 2, other);
    let handler = c.label(); c.a.mov_n(8, 13); c.a.extui(2, 13, 0, 1); c.zero(ccz::EQ, 2, handler);
    c.a.addi(8, 8, -1); put(c, RETRYING, 0); put(c, LIVE, 1); put(c, STATE, u32::from(net::CONNECTING));
    queue(c, n, watch::NET_OPEN); mark(c, l); c.jump(TURN);
    c.bind(handler); put(c, PENDING, 0); config(c, n, link);
    c.a.l32i(12, 7, 0); c.a.l32i(2, 7, 4); c.jump(enter);
    c.bind(other);
}

// The FC1 detail and grant are not word aligned. Read a little-endian word
// from a6 into a4, leaving the payload and row pointers intact.
fn word(c: &mut Code<'_>, offset: u32) {
    c.a.l8ui(4, 6, offset);
    for i in 1..4 { c.a.l8ui(5, 6, offset + i); c.shl(5, 5, 8 * i); c.a.or(4, 4, 5); }
}

fn status(c: &mut Code<'_>, offset: u32, bad: usize) {
    c.a.l8ui(2, 6, offset); c.imm(5, u32::from(net::READY)); c.branch(cc::LTU, 5, 2, bad);
    c.a.l8ui(3, 6, offset + 1); c.imm(5, u32::from(net::code::LAST)); c.branch(cc::LTU, 5, 3, bad);
    word(c, offset + 2);
}

fn state(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, bad: usize, done: usize) {
    status(c, 1, bad); // state, code, detail in a2, a3, a4
    c.a.l32i(5, 8, OPEN_SENT); c.zero(ccz::EQ, 5, done);
    let live = c.label(); c.a.l32i(5, 8, CLOSING); c.zero(ccz::EQ, 5, live);
    c.zero(ccz::NE, 2, done); put(c, OPEN_SENT, 0); put(c, CLOSING, 0);
    c.a.l32i(5, 8, WANTED); c.zero(ccz::EQ, 5, done);
    put(c, LIVE, 1); queue(c, n, watch::NET_OPEN); c.jump(done);
    c.bind(live);
    let offline = c.label(); c.zero(ccz::EQ, 2, offline);
    c.imm(5, u32::from(net::READY)); c.branch(cc::NE, 2, 5, done);
    c.a.l32i(5, 8, STATE); c.branch(cc::EQ, 2, 5, done);
    c.a.s32i(2, 8, STATE); clear_data(c); put(c, BACKOFF, FIRST); mark(c, l); c.jump(done);
    c.bind(offline); c.a.s32i(3, 8, ERROR); c.a.s32i(4, 8, DETAIL);
    end_ready(c); clear_data(c); put(c, OPEN_SENT, 0); put(c, LIVE, 0); put(c, STATE, u32::from(net::OFFLINE));
    retry_after(c, l); mark(c, l); c.jump(done);
}

// Copy a nonempty part, a2 -> a3, length a4. Preserve a6 and a8.
pub(super) fn copy_bytes(c: &mut Code<'_>) {
    let again = c.label(); let skip = c.label(); c.bind(again);
    c.a.l8ui(5, 2, 0); c.a.s8i(5, 3, 0); c.a.addi(2, 2, 1); c.a.addi(3, 3, 1); c.a.addi(4, 4, -1);
    c.a.extui(5, 4, 0, 6); c.zero(ccz::NE, 5, skip); c.feed(); c.bind(skip); c.zero(ccz::NE, 4, again);
}

// A ready endpoint may send without the program calling a data port. Keep
// its part in the declared receive slots, publishing only after CRC, epoch,
// flags, order and capacity have all been checked.
fn data(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, link: &Link, refused: usize) {
    c.a.extui(2, 3, net::PART_FIRST.trailing_zeros() as u8, 1); c.a.l32i(5, 8, RX_OPEN);
    c.branch(cc::EQ, 2, 5, refused);
    // A locally emptied slot is still the host's outstanding part until its
    // Freed enters the wire. Validate against the advertised slot balance.
    config(c, n, link); c.a.l32i(4, 7, 12); c.a.l32i(5, 8, RX_USED);
    c.a.l32i(2, 8, OWED); c.a.add_n(2, 2, 5); c.branch(cc::GEU, 2, 4, refused);
    c.a.addi(5, 5, 1); c.a.s32i(5, 8, RX_USED);
    c.a.l32i(5, 8, RX_WRITE); c.a.addi(2, 5, 1); let next = c.label();
    c.branch(cc::LTU, 2, 4, next); c.imm(2, 0); c.bind(next); c.a.s32i(2, 8, RX_WRITE);
    c.imm(2, SLOT_BYTES); c.a.mull(5, 5, 2); c.a.l32i(7, 7, 8); c.a.add_n(7, 7, 5);
    c.a.s8i(3, 7, 2); c.a.extui(2, 3, net::PART_LAST.trailing_zeros() as u8, 1);
    c.imm(5, 1); c.a.xor(2, 2, 5); c.a.s32i(2, 8, RX_OPEN);
    c.a.addi(4, 9, -2); c.a.s16i(4, 7, 0);
    c.a.addi(2, 6, 2); c.a.addi(3, 7, 4); copy_bytes(c); mark(c, l);
}

// rt/src/net/mod.rs:233-259. a7 kind, a6 payload, a9 length. Every change
// happens here after validation, independently of the carrier.
fn apply(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, link: &Link, bad: usize, refused: usize,
    done: usize, link_received: usize) {
    for (kind, size) in [(watch::NET_LINK, 6), (watch::NET_STATE, 7), (watch::NET_CREDIT, 5), (watch::NET_DATA, 3)] {
        let next = c.label(); c.imm(5, u32::from(kind)); c.branch(cc::NE, 7, 5, next);
        c.imm(5, size);
        if kind == watch::NET_DATA {
            c.branch(cc::LTU, 9, 5, bad); c.imm(5, net::NET_PART as u32 + 2); c.branch(cc::LTU, 5, 9, bad);
        } else { c.branch(cc::NE, 9, 5, bad); }
        if kind == watch::NET_LINK {
            status(c, 0, bad); c.store(2, n.link_state); c.store(3, n.link_state + 4); c.store(4, n.link_state + 8);
            rows(c, n, |c| mark(c, l));
        } else {
            c.a.l8ui(2, 6, 0); c.imm(5, n.connections.len() as u32); c.branch(cc::GEU, 2, 5, refused);
            row(c, n, 2);
            if kind == watch::NET_STATE { state(c, l, n, bad, done); }
            else {
                if kind == watch::NET_CREDIT { word(c, 1); c.zero(ccz::EQ, 4, bad); }
                else {
                    c.a.l8ui(3, 6, 1); c.imm(5, u32::from(net::PART_BINARY | net::PART_FIRST | net::PART_LAST));
                    c.branch(cc::LTU, 5, 3, bad);
                }
                c.a.l32i(2, 8, STATE); c.imm(5, u32::from(net::READY)); c.branch(cc::NE, 2, 5, done);
                if kind == watch::NET_CREDIT {
                    c.imm(5, 0);
                    c.a.l32i(2, 8, GRANT); c.a.l32i(3, 8, GRANT + 4); c.add64(2, 3, 4, 5, 6, 7);
                    c.a.s32i(2, 8, GRANT); c.a.s32i(3, 8, GRANT + 4);
                    if n.tx.is_some() { data::reserve(c, n); }
                    mark(c, l);
                } else { data(c, l, n, link, refused); }
            }
        }
        c.jump(if kind == watch::NET_LINK { link_received } else { done }); c.bind(next);
    }
    c.jump(bad);
}

pub(super) mod tether {
    use super::*;

    // A refused Data is still at the front. A 64-byte USB drain rarely
    // makes room for a full part: avoid copying its bytes at each wake.
    // WIRE's COBS reservation covers the six-byte header, c, flags and CRC.
    pub(super) fn writable(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, e: &LinkSram, blocked: usize) {
        c.load(2, e.rehello); c.zero(ccz::NE, 2, blocked);
        if let Some(tx) = &n.tx {
            let fresh = c.label(); c.load(2, tx.part_bytes); c.zero(ccz::EQ, 2, fresh); c.call(crate::runtime::DRAIN);
            c.load(2, tx.part_bytes); c.a.addi(2, 2, 8); crate::runtime::wire_room(c, l, blocked); c.bind(fresh);
        }
    }

    // FC1 header space followed by the payload of rt/src/net/mod.rs:205-229.
    // a2 payload length, a3 body address. Return zero on backpressure.
    pub(super) fn send(c: &mut Code<'_>, l: &SramLayout) {
        c.a.addi(2, 2, 6); c.load(4, l.stamp); c.call(WIRE);
    }

    pub(in crate::runtime) fn receive(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, e: &LinkSram, link: &Link,
        bad: usize, refused: usize, stale: usize, done: usize) {
        let other = c.label(); c.imm(5, u32::from(watch::NET_OPEN)); c.branch(cc::LTU, 3, 5, other);
        c.imm(5, u32::from(watch::NET_LINK)); c.branch(cc::LTU, 5, 3, other);
        c.load(9, e.rx_bytes); c.imm(5, 7); c.branch(cc::LTU, 9, 5, bad); c.a.addi(9, 9, -7);
        c.a.mov_n(7, 3); crate::runtime::rx_epoch(c, e, stale); c.address(6, n.rx_payload);
        let link_received = if l.eth.is_some() { c.label() } else { done };
        apply(c, l, n, link, bad, refused, done, link_received);
        if let Some(eth) = &l.eth {
            c.bind(link_received); crate::runtime::link_answered(c, eth, e); c.jump(done);
        }
        c.bind(other);
    }
}

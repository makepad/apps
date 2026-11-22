//! Send bytes, message boundaries and receive slots of one connection.
use super::*;
use crate::image::NetTx;
use crate::runtime::BUFFER_ROW_SHIFT;

// A port owns the existing buffer scratch for its call. Flushing the wire
// can run inside that call and uses only the connection row and NetTx.
const BUFFER: u32 = 0;
const REQUEST: u32 = 4;
const FLAGS: u32 = 8;
const MOVED: u32 = 12;
const CONNECTION: u32 = 16;
const REMAINING: u32 = 20;
const START: u32 = 24;
const WAS_OPEN: u32 = 28;
const _: () = assert!(WAS_OPEN + 4 <= crate::runtime::BUFFER_SCRATCH_BYTES);

// a6 bytes become eligible now. Reserve grant once, in call/arrival order.
// Keep the final byte of an unfinished message until its LAST is known.
pub(super) fn reserve(c: &mut Code<'_>, n: &NetSram) {
    let done = c.label(); let closed = c.label(); let fits = c.label();
    c.a.l32i(6, 8, TX_USED); c.a.l32i(3, 8, RESERVED); c.a.sub(6, 6, 3);
    c.a.l32i(2, 8, TX_OPEN); c.zero(ccz::EQ, 2, closed); c.a.addi(6, 6, -1); c.bind(closed);
    c.a.l32i(4, 8, GRANT); c.a.l32i(5, 8, GRANT + 4); c.zero(ccz::NE, 5, fits);
    c.a.minu(6, 6, 4); c.bind(fits); c.zero(ccz::EQ, 6, done);
    let no_borrow = c.label(); c.branch(cc::GEU, 4, 6, no_borrow); c.a.addi(5, 5, -1); c.bind(no_borrow);
    c.a.sub(4, 4, 6); c.a.s32i(4, 8, GRANT); c.a.s32i(5, 8, GRANT + 4);
    c.a.add_n(3, 3, 6); c.a.s32i(3, 8, RESERVED); queue(c, n, watch::NET_DATA); c.bind(done);
}

// For eight consecutive send bytes the side table has three bytes of bits:
// BINARY, FIRST, LAST. a7 is the flash configuration; a6 the ring index.
fn group(c: &mut Code<'_>) {
    c.a.srli(3, 6, 3); c.a.addx2(3, 3, 3); c.a.l32i(5, 7, 24); c.a.add_n(3, 3, 5);
    c.a.extui(5, 6, 0, 3); c.a.ssl(5);
}

fn set_boundary(c: &mut Code<'_>, plane: u32) {
    group(c); c.imm(5, 1); c.a.sll(5, 5); c.a.l8ui(4, 3, plane); c.a.or(4, 4, 5); c.a.s8i(4, 3, plane);
}

// Copy the taken range, resetting just those bits even when another message
// shares its first or last bitmap byte. Each run ends at a group or ring edge.
fn append(c: &mut Code<'_>, s: u32) {
    let again = c.label(); c.bind(again);
    c.a.l32i(6, 8, TX_HEAD); c.a.l32i(2, 7, 20); c.a.sub(2, 2, 6);
    c.a.extui(4, 6, 0, 3); c.imm(5, 8); c.a.sub(5, 5, 4); c.a.minu(2, 2, 5);
    c.load(4, s + REMAINING); c.a.minu(2, 2, 4); c.a.sub(4, 4, 2); c.store(4, s + REMAINING);
    c.imm(5, 1); c.a.ssl(2); c.a.sll(4, 5); c.a.addi(4, 4, -1);
    group(c); c.a.sll(4, 4); c.imm(5, u32::MAX); c.a.xor(5, 5, 4);
    for plane in 0..3 {
        c.a.l8ui(9, 3, plane); c.a.and(9, 9, 5);
        if plane == 0 {
            let text = c.label(); c.a.l32i(10, 8, TX_OPEN); c.a.extui(10, 10, 0, 1);
            c.zero(ccz::EQ, 10, text); c.a.or(9, 9, 4); c.bind(text);
        }
        c.a.s8i(9, 3, plane);
    }
    c.a.l32i(3, 7, 16); c.a.add_n(3, 3, 6);
    c.a.add_n(6, 6, 2); c.a.l32i(5, 7, 20);
    let fits = c.label(); c.branch(cc::LTU, 6, 5, fits); c.imm(6, 0); c.bind(fits); c.a.s32i(6, 8, TX_HEAD);
    c.a.mov_n(4, 2); c.load(2, s + BUFFER); copy_bytes(c); c.store(2, s + BUFFER);
    c.load(4, s + REMAINING); c.zero(ccz::NE, 4, again);
}

fn send(c: &mut Code<'_>, n: &NetSram, s: u32) {
    let done = c.label(); let finish = c.label(); let drain = c.label();
    c.a.l32i(2, 8, STATE); c.imm(3, u32::from(net::READY)); c.branch(cc::NE, 2, 3, done);
    c.load(4, s + REQUEST); c.zero(ccz::EQ, 4, finish);
    c.a.l32i(5, 8, TX_USED); c.a.l32i(2, 7, 20); c.a.sub(2, 2, 5); c.a.minu(2, 2, 4);
    c.store(2, s + MOVED); c.store(2, s + REMAINING);
    let taking = c.label(); c.branch(cc::EQ, 2, 4, taking);
    c.a.l32i(3, 8, RESERVED); c.zero(ccz::EQ, 3, taking); put(c, WAIT_SPACE, 1); c.bind(taking);
    c.zero(ccz::EQ, 2, drain); c.a.l32i(5, 8, TX_USED); c.a.add_n(5, 5, 2); c.a.s32i(5, 8, TX_USED);
    c.a.l32i(3, 8, TX_OPEN); c.store(3, s + WAS_OPEN);
    let continuing = c.label(); c.zero(ccz::NE, 3, continuing);
    c.load(3, s + FLAGS); c.a.extui(3, 3, 0, 1); c.a.addi(3, 3, i32::from(net::PART_FIRST)); c.a.s32i(3, 8, TX_OPEN);
    c.bind(continuing); c.a.l32i(6, 8, TX_HEAD); c.store(6, s + START);
    append(c, s);
    let appended = c.label(); c.load(2, s + WAS_OPEN); c.zero(ccz::NE, 2, appended);
    c.load(6, s + START); set_boundary(c, 1); c.bind(appended);
    c.load(2, s + MOVED); c.load(3, s + REQUEST); c.branch(cc::NE, 2, 3, drain);
    c.bind(finish); c.a.l32i(2, 8, TX_OPEN); c.zero(ccz::EQ, 2, done);
    c.load(2, s + FLAGS); c.a.extui(2, 2, net::SEND_MORE.trailing_zeros() as u8, 1); c.zero(ccz::NE, 2, drain);
    c.a.l32i(6, 8, TX_HEAD); let previous = c.label(); c.zero(ccz::NE, 6, previous);
    c.a.l32i(6, 7, 20); c.bind(previous); c.a.addi(6, 6, -1); set_boundary(c, 2); put(c, TX_OPEN, 0);
    c.bind(drain); reserve(c, n); c.call(NET_FLUSH);
    // If the physical ring still owns the missing room, wake the writer
    // when it is released, even if no further grant comes from the host.
    c.load(8, s + CONNECTION); c.load(2, s + MOVED); c.load(3, s + REQUEST); c.branch(cc::EQ, 2, 3, done);
    c.a.l32i(2, 8, RESERVED); c.zero(ccz::EQ, 2, done); put(c, WAIT_SPACE, 1); c.bind(done);
}

// Prepare one wire part without advancing any cursor. The shared ticket
// can span several completed messages, a ring wrap, or more than NET_PART.
pub(super) fn part(c: &mut Code<'_>, n: &NetSram, tx: &NetTx, link: &Link) {
    config(c, n, link); c.imm(2, net::NET_PART as u32); c.a.minu(2, 6, 2);
    c.a.l32i(6, 8, TX_TAIL); group(c); c.imm(5, 1); c.a.sll(5, 5);
    c.a.l8ui(4, 3, 0); c.a.and(4, 4, 5); c.imm(9, u32::from(net::PART_BINARY)); c.imm(10, 0); c.a.movnez(10, 9, 4);
    c.a.l8ui(4, 3, 1); c.a.and(4, 4, 5); c.imm(9, u32::from(net::PART_FIRST)); c.a.movnez(4, 9, 4);
    c.a.or(10, 10, 4); c.byte(10, tx.body + 7);
    c.address(4, tx.body + 8);
    let again = c.label(); let last = c.label(); let done = c.label();
    c.bind(again); c.a.l32i(3, 7, 16); c.a.add_n(3, 3, 6); c.a.l8ui(5, 3, 0);
    c.a.s8i(5, 4, 0); c.a.addi(4, 4, 1);
    group(c); c.imm(5, 1); c.a.sll(5, 5); c.a.l8ui(3, 3, 2); c.a.and(3, 3, 5); c.zero(ccz::NE, 3, last);
    c.a.addi(2, 2, -1); c.zero(ccz::EQ, 2, done);
    c.a.addi(6, 6, 1); c.a.l32i(5, 7, 20); let fits = c.label(); c.branch(cc::LTU, 6, 5, fits); c.imm(6, 0); c.bind(fits);
    let fed = c.label(); c.a.extui(5, 2, 0, 5); c.zero(ccz::NE, 5, fed); c.feed(); c.bind(fed); c.jump(again);
    c.bind(last); c.address(3, tx.body + 7); c.a.l8ui(2, 3, 0); c.a.addi(2, 2, i32::from(net::PART_LAST)); c.a.s8i(2, 3, 0);
    c.bind(done); c.address(3, tx.body + 8); c.a.sub(2, 4, 3); c.store(2, tx.part_bytes); c.a.addi(2, 2, 2);
}

// Only a successful atomic wire enqueue releases these bytes.
pub(super) fn sent(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, tx: &NetTx, link: &Link, more: usize) {
    c.load(5, tx.part_bytes); c.set(tx.part_bytes, 0); c.a.sub(6, 6, 5); c.a.slli(3, 5, 10);
    c.a.l32i(2, 4, 0); c.a.sub(2, 2, 3); c.a.s32i(2, 4, 0);
    for off in [TX_USED, RESERVED] { c.a.l32i(2, 8, off); c.a.sub(2, 2, 5); c.a.s32i(2, 8, off); }
    config(c, n, link); c.a.l32i(2, 8, TX_TAIL); c.a.add_n(2, 2, 5); c.a.l32i(3, 7, 20);
    let fits = c.label(); c.branch(cc::LTU, 2, 3, fits); c.a.sub(2, 2, 3); c.bind(fits); c.a.s32i(2, 8, TX_TAIL);
    let quiet = c.label(); c.a.l32i(2, 8, WAIT_SPACE); c.zero(ccz::EQ, 2, quiet);
    put(c, WAIT_SPACE, 0); mark(c, l); c.bind(quiet); c.zero(ccz::NE, 6, more);
}

// a6 slot index -> a6 slot address, a7 flash configuration.
fn slot(c: &mut Code<'_>) {
    c.imm(5, SLOT_BYTES); c.a.mull(6, 6, 5); c.a.l32i(5, 7, 8); c.a.add_n(6, 6, 5);
}

fn next_slot(c: &mut Code<'_>) {
    c.a.addi(3, 3, 1); c.a.l32i(5, 7, 12);
    let fits = c.label(); c.branch(cc::LTU, 3, 5, fits); c.imm(3, 0); c.bind(fits);
}

pub(super) fn inspect(c: &mut Code<'_>, n: &NetSram, link: &Link, port: u16) {
    config(c, n, link); c.a.l32i(4, 8, RX_USED); c.a.l32i(3, 8, RX_TAIL);
    let done = c.label();
    if port == ports::NET_KIND {
        c.a.l32i(2, 8, RX_READING); let kind = c.label(); c.zero(ccz::NE, 2, kind);
        c.zero(ccz::EQ, 4, done); c.a.mov_n(6, 3); slot(c); c.a.l8ui(2, 6, 2);
        c.bind(kind); c.a.extui(2, 2, 0, 1);
    } else {
        let held = c.label(); c.zero(ccz::NE, 4, held); c.a.l32i(2, 8, RX_READING);
        c.zero(ccz::EQ, 2, done); c.imm(2, 1); c.jump(done);
        c.bind(held); c.a.l32i(2, 8, RX_OFFSET); c.a.neg(2, 2);
        let again = c.label(); c.bind(again); c.feed(); c.a.mov_n(6, 3); slot(c);
        c.a.l16ui(5, 6, 0); c.a.add_n(2, 2, 5); c.a.l8ui(5, 6, 2);
        c.a.extui(5, 5, net::PART_LAST.trailing_zeros() as u8, 1); c.zero(ccz::NE, 5, done);
        next_slot(c); c.a.addi(4, 4, -1); c.zero(ccz::NE, 4, again); c.a.addi(2, 2, 1);
    }
    c.bind(done);
}

fn recv(c: &mut Code<'_>, n: &NetSram, s: u32) {
    let again = c.label(); let done = c.label();
    c.bind(again); c.a.l32i(2, 8, RX_USED); c.zero(ccz::EQ, 2, done);
    c.load(4, s + REQUEST); c.load(5, s + MOVED); c.a.sub(4, 4, 5); c.zero(ccz::EQ, 4, done);
    c.a.l32i(6, 8, RX_TAIL); slot(c);
    c.a.l32i(2, 8, RX_READING); let reading = c.label(); c.zero(ccz::NE, 2, reading);
    c.a.l8ui(2, 6, 2); c.a.extui(2, 2, 0, 1); c.a.addi(2, 2, i32::from(net::PART_FIRST)); c.a.s32i(2, 8, RX_READING);
    c.bind(reading); c.a.l32i(2, 8, RX_OFFSET); c.a.l16ui(3, 6, 0); c.a.sub(3, 3, 2); c.a.minu(4, 4, 3);
    c.a.add_n(3, 2, 4); c.a.s32i(3, 8, RX_OFFSET); c.a.add_n(2, 2, 6); c.a.addi(2, 2, 4);
    c.load(5, s + MOVED); c.load(3, s + BUFFER); c.a.add_n(3, 3, 5); c.a.add_n(5, 5, 4); c.store(5, s + MOVED);
    copy_bytes(c);
    c.a.l32i(2, 8, RX_OFFSET); c.a.l16ui(3, 6, 0); c.branch(cc::NE, 2, 3, done);
    put(c, RX_OFFSET, 0); c.a.l32i(3, 8, RX_TAIL); next_slot(c); c.a.s32i(3, 8, RX_TAIL);
    c.a.l32i(3, 8, RX_USED); c.a.addi(3, 3, -1); c.a.s32i(3, 8, RX_USED);
    c.a.l32i(2, 8, OWED); c.a.addi(3, 2, 1); c.a.s32i(3, 8, OWED);
    let owed = c.label(); c.zero(ccz::NE, 2, owed); queue(c, n, watch::NET_CREDIT); c.bind(owed);
    c.a.l8ui(2, 6, 2); c.a.extui(2, 2, net::PART_LAST.trailing_zeros() as u8, 1); c.zero(ccz::EQ, 2, again);
    put(c, RX_READING, 0); c.bind(done);
    c.a.l32i(2, 8, OWED); let quiet = c.label(); c.zero(ccz::EQ, 2, quiet); c.call(NET_FLUSH); c.bind(quiet);
}

pub(super) fn transfer(c: &mut Code<'_>, l: &SramLayout, n: &NetSram, link: &Link, port: u16) {
    // Loader verifies U8 and writability. Check off and n before inspecting
    // readiness or moving a cursor. Unsigned comparisons also reject negatives.
    let s = l.buffer_scratch; let bad = c.label(); let returned = c.label();
    c.store(0, l.port_return); c.shl(3, 3, BUFFER_ROW_SHIFT); c.address(7, link.buffers); c.a.add_n(3, 7, 3);
    c.a.l32i(2, 3, 4); c.branch(cc::LTU, 2, 4, bad); c.a.sub(2, 2, 4); c.branch(cc::LTU, 2, 5, bad);
    c.a.l32i(2, 3, 0); c.a.add_n(2, 2, 4);
    c.store(5, s + REQUEST); c.set(s + MOVED, 0);
    if port == ports::NET_SEND {
        c.store(6, s + FLAGS); c.store(2, s + BUFFER); c.store(8, s + CONNECTION);
        config(c, n, link); send(c, n, s);
    } else {
        c.store(2, s + BUFFER); config(c, n, link); recv(c, n, s);
    }
    c.load(2, s + MOVED); c.jump(returned);
    c.bind(bad); c.a.mov_n(5, 4); c.a.mov_n(4, 10); c.imm(3, u32::from(watch::FAULT_NET_RANGE));
    c.imm(2, u32::from(watch::FAULT)); c.call(EVENT); c.imm(2, 0);
    c.bind(returned); c.load(0, l.port_return);
}

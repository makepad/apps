//! Network ports over the S3's USJ, with the same Host and World as the interpreter.
use std::sync::Arc;

use fab_elec_mcu::Compiled;
use fab_elec_mcu_emit_s3::{build_slot, chip, Image, SlotOptions, SLOT_BASE};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Until};
use fab_elec_mcu_rt::{module, net::{self, code, Condition, Endpoint, Host, NetMsg, Outcome, Responder, Rules, Then, World}, watch};
use fab_elec_mcu_s3::S3Machine;

#[path = "support/link_image.rs"]
mod link_image;
#[path = "support/net_link.rs"]
mod net_link;
#[path = "../../lang/tests/support/net_reference.rs"]
mod net_reference;

use net_link::NetLink;

const MS: u64 = 1_000_000;

struct Rig {
    compiled: Compiled,
    image: Image,
    machine: S3Machine,
    link: NetLink,
    ms: u64,
}

impl Rig {
    fn new(source: &str, world: World) -> Self {
        let compiled = link_image::compiled(source);
        let image = link_image::image(&compiled);
        let slots: Vec<_> = image.slot.layout.net.as_ref().unwrap().connections.iter()
            .map(|c| u16::try_from(c.slot_count).expect("slot count fits physical SRAM")).collect();
        let machine = S3Machine::new(Arc::new(image.flash.clone()), &link_image::profile());
        Self { compiled, image, machine, link: NetLink::new(Host::new(world, &slots)), ms: 0 }
    }

    fn run(&mut self, ticks: u64, stop: Option<u32>) {
        let end = self.machine.now() + ticks;
        let mut marks = PcSet::default();
        if let Some(pc) = stop {
            marks = PcSet::new(SLOT_BASE, self.image.slot.bytes.len() as u32); marks.insert(pc);
        }
        let mut barrier = None;
        loop {
            let ran = self.machine.run(&Until { tick: end, marks: &marks, stops: stop.as_slice(),
                stops_after: self.machine.now(), on_drive: false });
            match ran {
                Ran::Barrier => {
                    let now = self.machine.now();
                    assert_ne!(barrier.replace(now), Some(now), "repeated barrier without advancing");
                    self.machine.mark();
                }
                Ran::Pc if stop == Some(self.machine.pc()) => break,
                Ran::Tick if stop.is_none() => break,
                other => panic!("network image missed its target: {other:?}, {:?}", self.machine.status()),
            }
        }
    }

    fn until_pc(&mut self, pc: u32) { self.run(self.machine.tick_hz() / 2, Some(pc)); }

    fn word(&self, address: u32) -> u32 {
        let mut bytes = [0; 4]; assert!(self.machine.read_data(address, &mut bytes)); u32::from_le_bytes(bytes)
    }

    fn var(&self, name: &str) -> u32 {
        let v = self.compiled.lowered.state_vars.iter().find(|v| v.name == name).unwrap();
        self.word(self.image.slot.layout.state + 4 * v.offset)
    }

    fn until_ms(&mut self, end: u64) {
        while self.ms < end {
            self.run(self.machine.tick_hz() / 1000, None);
            self.ms += 1;
            self.link.exchange(&mut self.machine, self.ms * MS);
        }
    }
}

#[test]
fn host_loss_and_rejections_reopen_on_the_device_backoff() {
    let world = World::new(7, vec![Endpoint { responder: Responder::Sink }], vec![
        (50 * MS, Condition::LinkDown { code: code::NO_NETWORK, detail: 201 }),
        (70 * MS, Condition::Join { ns: 2 * MS, fails: None }),
        (70 * MS, Condition::Open { c: 0, outcome: Outcome::Rejected(403) }),
        (30_000 * MS, Condition::Open { c: 0, outcome: Outcome::Ready }),
        (35_500 * MS, Condition::Disconnect { c: 0, code: code::CLOSED, detail: 1001 }),
    ]);
    let mut r = Rig::new(r#"
var initial: i32 = -1
var opened: i32 = 0
var closed: i32 = 0
var current: i32 = -1
var epoch: i32 = 0
var error: i32 = 0
var detail: i32 = 0
var link_state: i32 = -1
var link_code: i32 = 0
var link_detail: i32 = 0
let conn = net(net_websocket, 2, 32, fn() {
    link_state = net_link()
    link_code = net_link_error()
    link_detail = net_link_detail()
    let state = net_state(conn)
    epoch = net_epoch(conn)
    error = net_error(conn)
    detail = net_detail(conn)
    if state != current { probe(3, state) }
    current = state
})
let finish = after(ms(37000), fn() {
    closed = net_close(conn)
    closed = 10 * closed + net_close(conn)
    current = net_state(conn)
    epoch = net_epoch(conn)
    error = net_error(conn)
    detail = net_detail(conn)
})
fn setup() {
    initial = net_link() + net_state(conn)
    opened = net_open(conn)
    opened = 10 * opened + net_open(conn)
    current = net_state(conn)
}
"#, world);
    r.until_pc(r.image.slot.symbols.dispatch);
    assert_eq!((r.var("initial"), r.var("opened"), r.var("current")), (0, 10, 1), "reset, one wanted open, connecting");
    r.until_ms(30);
    assert_eq!((r.var("link_state"), r.var("current"), r.var("epoch")), (2, 2, 0), "the host made the connection ready");
    r.until_ms(65);
    assert_eq!((r.var("link_state"), r.var("link_code"), r.var("link_detail")), (0, u32::from(code::NO_NETWORK), 201));
    assert_eq!((r.var("current"), r.var("epoch"), r.var("error"), r.var("detail")), (0, 1, u32::from(code::LINK_LOST), 201));
    r.until_ms(1100);
    assert_eq!((r.var("current"), r.var("epoch"), r.var("error"), r.var("detail")), (0, 1, u32::from(code::REJECTED), 403));
    r.until_ms(35_400);
    assert_eq!((r.var("current"), r.var("epoch")), (2, 1), "failed attempts did not end another ready period");
    r.until_ms(35_550);
    assert_eq!((r.var("epoch"), r.var("error"), r.var("detail")), (2, u32::from(code::CLOSED), 1001));
    r.until_ms(36_600);
    assert_eq!((r.var("current"), r.var("epoch")), (2, 2), "ready reset the backoff");

    let ends: Vec<_> = r.link.probes.iter().filter_map(|&(t, id, state)| (id == 3 && state == 0).then_some(t)).collect();
    let opens: Vec<_> = r.link.up.iter().map(|(t, m)| { assert_eq!(*m, NetMsg::Open { c: 0 }); *t }).collect();
    let waits = [1, 2, 4, 8, 10, 10, 1];
    assert_eq!((ends.len(), opens.len()), (waits.len(), waits.len() + 1), "one open per attempt");
    for ((&offline, &open), seconds) in ends.iter().zip(&opens[1..]).zip(waits) {
        let delay = open.wrapping_sub(offline);
        // Both watch stamps are on the device's 16 MHz grid. Allow one USB
        // service period for dispatch and enqueue after the deadline.
        let expected = seconds * 16_000_000;
        assert!((expected..=expected + 16_000).contains(&delay), "{seconds} s backoff took {delay} ticks");
    }
    r.until_ms(38_500);
    assert_eq!((r.var("closed"), r.var("current"), r.var("epoch"), r.var("error"), r.var("detail")),
        (10, 0, 3, 0, 0), "closing a ready connection ends its epoch and clears its codes");
    assert_eq!(r.link.up.len(), opens.len() + 1, "the explicit close cancelled reopening");
    assert_eq!(r.link.up.last().unwrap().1, NetMsg::Close { c: 0 });
    assert!(r.link.faults.is_empty(), "unexpected faults: {:?}", r.link.faults);
}

#[test]
fn full_tx_retries_controls_once_in_call_order() {
    let world = World::new(3, vec![Endpoint { responder: Responder::Sink }; 2], vec![]);
    let mut r = Rig::new(r#"
var result: i32 = -1
var accepted: i32 = 0
var a_state: i32 = 0
var b_state: i32 = 0
let a = net(net_tcp, 2, 1, fn() { a_state = net_state(0) })
let b = net(net_tcp, 2, 1, fn() { b_state = net_state(1) })
fn setup() {
    result = net_state(net_link() - 1)
    for i in 0..800 { probe(1, i) }
    accepted = accepted + net_open(1)
    accepted = accepted + net_open(0)
    accepted = accepted + net_close(1)
    accepted = accepted + net_open(1)
}

"#, world);
    r.machine.set_usb_host_listening(false);
    let open = r.image.slot.map.runtime.iter().find(|f| f.name == "net_open").unwrap().start;
    r.until_pc(open);
    let l = &r.image.slot.layout;
    let seq = l.seq; let lost = l.lost_count;
    let before = r.word(seq); let lost_probes = r.word(lost);
    let room = r.word(l.tx_tail).wrapping_sub(r.word(l.tx_head)).wrapping_sub(1)
        & (fab_elec_mcu_emit_s3::runtime::TX_RING_BYTES - 1);
    assert!(room < 12 && lost_probes > 0, "the ring refused a twelve-byte Probe");
    r.until_pc(r.image.slot.symbols.dispatch);
    assert_eq!((r.var("accepted"), r.var("result")), (4, 0), "control calls return while USB is blocked");
    assert!(r.word(seq) - before < 2, "at least one Open could not enter the wire ring");
    assert_eq!(r.word(lost), lost_probes, "deferred controls do not count as lost watch events");
    r.machine.set_usb_host_listening(true);
    r.until_ms(180);
    let sent: Vec<_> = r.link.up.iter().map(|(_, m)| m.clone()).collect();
    assert_eq!(sent, [NetMsg::Open { c: 1 }, NetMsg::Open { c: 0 }, NetMsg::Close { c: 1 }, NetMsg::Open { c: 1 }],
        "both opens, the close, then its acknowledged reopen");
    assert_eq!((r.var("a_state"), r.var("b_state")), (2, 2), "both connections recovered");
    assert!(matches!(r.link.faults.as_slice(), [(watch::FAULT_NET_HANDLE, _, -1)]), "the invalid handle fault: {:?}", r.link.faults);

    let refused = r.image.slot.layout.link.as_ref().unwrap().rx_refused;
    let before = r.word(refused);
    r.link.send_down(&mut r.machine, &NetMsg::State { c: 2, state: net::OFFLINE, code: code::RESET, detail: 0 });
    for byte in 0..3 {
        r.link.send_down(&mut r.machine, &NetMsg::Data { c: 0, flags: net::PART_FIRST | net::PART_LAST, bytes: vec![byte] });
    }
    r.until_ms(185);
    assert_eq!(r.word(refused) - before, 2, "one unknown connection and one part without a slot");
    assert_eq!((r.var("a_state"), r.var("b_state")), (2, 2), "refusals preserved both connections");
}

#[test]
fn ethernet_wins_a_tie_and_its_full_ring_still_admits_connection_control() {
    let mut r = Rig::new(r#"
var order: i32 = 0
var why: i32 = 0
let wire = eth(1, fn() { order = 10 * order + 1 })
let conn = net(net_tcp, 2, 1, fn() {
    if net_link() == net_joining { order = 10 * order + 2 }
    why = net_link_detail()
})
"#, World::new(11, vec![], vec![]));
    r.until_pc(r.image.slot.symbols.dispatch); r.until_ms(5);
    assert_eq!(r.word(r.image.slot.layout.eth.as_ref().unwrap().answer_state), 2,
        "the host's Link acknowledges the Hello even before an Ethernet packet");
    // Both fit in one OUT packet. Put NET first on the wire so arrival
    // order alone cannot make the Ethernet handler win their equal stamp.
    r.link.send_down(&mut r.machine, &NetMsg::Link { state: net::LINK_JOINING, code: code::NONE, detail: 0 });
    let mut wire = [0; watch::ETH_WIRE];
    let len = watch::encode_eth(r.link.seq, r.link.hello.unwrap().tick, &[0x55; watch::ETH_MIN], &mut wire).unwrap();
    r.link.seq = r.link.seq.wrapping_add(1);
    for &b in &wire[..len] { r.machine.link_in(0, b); }
    r.until_ms(10);
    assert_eq!(r.var("order"), 12, "Ethernet ran first and left its only slot full");
    r.link.send_down(&mut r.machine, &NetMsg::Link { state: net::LINK_NONE, code: code::LINK_LOST, detail: 0x8765_4321 });
    r.until_ms(15);
    assert_eq!(r.var("why"), 0x8765_4321, "link control still carries all four detail bytes");
    assert_eq!(r.word(r.image.slot.layout.link.as_ref().unwrap().rx_refused), 0);
}

#[test]
fn receive_slots_and_send_bytes_share_the_image_sram_budget() {
    for (slots, send) in [(1_000_000, 1), (2, 1_000_000)] {
        let c = link_image::compiled(&format!("let conn = net(net_tcp, {slots}, {send}, fn() {{}})"));
        let m = module::load(&c.module, &chip::profile()).unwrap();
        let error = build_slot(&m, &SlotOptions::default()).err().expect("queues exceed internal SRAM");
        assert!(error.message.contains("internal SRAM") && error.message.contains("deficit"), "{}", error.message);
    }
}

const CLIENT: &str = include_str!("../../lang/tests/support/net.splash");

#[test]
fn the_same_client_and_world_agree_across_a_disconnect_and_reopen() {
    let source = CLIENT.replace("var goal: i32 = 0", "var goal: i32 = 193");
    let pattern = |n: usize| (0..n).map(|i| (7 * i + 3) as u8).collect::<Vec<u8>>();
    let world = World::new(17, vec![Endpoint { responder: Responder::Rules(Rules::at_ready(vec![
        Then::message(15 * MS, true, pattern(137)), Then::message(23 * MS, false, pattern(71)),
    ]).into()) }], vec![
        (0, Condition::Latency { c: 0, ns: 4 * MS, jitter_ns: 0 }),
        (120 * MS, Condition::Disconnect { c: 0, code: code::CLOSED, detail: 1001 }),
    ]);
    let end = 1250;
    let reference = net_reference::run(&source, world.clone(), end * MS);
    let mut r = Rig::new(&source, world);
    r.until_pc(r.image.slot.symbols.dispatch); r.until_ms(end);
    assert!(reference.faults.is_empty() && r.link.faults.is_empty(), "faults: {:?}, {:?}", reference.faults, r.link.faults);
    assert_eq!((reference.stale, reference.host_stale, r.link.host.stale), (0, 0, 0));
    let values: Vec<_> = r.link.probes.iter().map(|&(_, id, value)| (id, value)).collect();
    assert_eq!(values, reference.probes.iter().map(|&(_, id, value)| (id, value)).collect::<Vec<_>>(),
        "the complete ordered observations include codes, raw detail, epoch, and checked message bytes and kinds");
    let lengths: Vec<_> = values.iter().filter_map(|&(id, v)| (id == 9).then_some(v)).collect();
    assert_eq!(lengths, [137, 71, 137, 71], "both messages completed in each ready period");
    assert!(values.contains(&(7, 1)) && values.contains(&(6, 1001)), "the program observed the loss");
    let up: Vec<_> = r.link.up.iter().map(|(_, m)| m).collect();
    assert_eq!(up, reference.wire.iter().filter_map(|(_, sent, m)| sent.then_some(m)).collect::<Vec<_>>(),
        "the complete upstream bytes, flags, returned slots and opens");
    // This fixture has a 64-byte USB packet each ms. At most four packets
    // for the 193-byte send, three for the 137-byte reply, and three for a
    // burst of probes. Six further periods cover Hello, loss, reopen and
    // ready crossing the two service loops and setup on the device clock.
    for (&(tick, id, value), &(ns, _, _)) in r.link.probes.iter().zip(&reference.probes) {
        let device_ns = u64::from(tick) * 1000 / 16;
        assert!(device_ns.abs_diff(ns) <= 16 * MS, "probe {id}={value}: S3 {device_ns} ns, reference {ns} ns");
    }
}

#[test]
fn echo_exceeds_the_initial_grant_and_both_receive_slots() {
    let size = (net::FIRST_GRANT + net::GRANT_STEP + 19) as usize;
    let source = CLIENT.replace("var goal: i32 = 0", &format!("var goal: i32 = {size}"))
        .replace("var step: i32 = 1600", "var step: i32 = 521")
        .replace("net(net_websocket, 4, 4096,", "net(net_websocket, 2, 4096,");
    let world = World::new(7, vec![Endpoint { responder: Responder::Echo }],
        vec![(0, Condition::Rate { c: 0, bytes_per_s: 8192 })]);
    let mut r = Rig::new(&source, world);
    r.until_pc(r.image.slot.symbols.dispatch);
    // Both directions cross the world's 8192 byte/s link. Allow two more
    // USB traversals at 64 bytes/ms and the final control packets.
    r.until_ms(2 * (size as u64).div_ceil(8192) * 1000 + 2 * (size as u64).div_ceil(64) + 100);
    assert!(r.link.faults.is_empty(), "client faults: {:?}", r.link.faults);
    let reports: Vec<_> = r.link.probes.iter().filter(|p| (9..=12).contains(&p.1)).map(|p| (p.1, p.2)).collect();
    assert_eq!(reports, [(9, size as u16), (10, (size >> 16) as u16), (11, 0), (12, 1)],
        "the program received one whole binary message and checked every byte");
    assert!(r.link.probes.iter().any(|&(_, id, value)| id == 13 && value > 0), "a short send was retried");
    let parts: Vec<_> = r.link.up.iter().filter_map(|(_, m)| match m {
        NetMsg::Data { c: 0, bytes, flags } => Some((bytes, *flags)), _ => None,
    }).collect();
    let expected: Vec<_> = (0..size).map(|i| ((i % 1500) * 7 + 3) as u8).collect();
    let bytes: Vec<_> = parts.iter().flat_map(|(bytes, _)| bytes.iter().copied()).collect();
    assert_eq!(bytes, expected, "all upstream bytes stayed in order across short takes and ring wraps");
    for (i, (bytes, flags)) in parts.iter().enumerate() {
        let want = net::PART_BINARY | if i == 0 { net::PART_FIRST } else { 0 }
            | if i + 1 == parts.len() { net::PART_LAST } else { 0 };
        assert_eq!(*flags, want); assert!((1..=net::NET_PART).contains(&bytes.len()));
    }
    let grants = r.link.down.iter().filter(|m| matches!(m, NetMsg::Granted { .. })).count();
    assert!(grants >= 2, "the host granted more than its initial window");
    let freed: usize = r.link.up.iter().filter_map(|(_, m)| match m { NetMsg::Freed { slots, .. } => Some(usize::from(*slots)), _ => None }).sum();
    assert_eq!(freed, size.div_ceil(net::NET_PART), "each received part returned one slot");
    let link = r.image.slot.layout.link.as_ref().unwrap();
    assert_eq!((r.link.host.stale, r.word(link.rx_bad), r.word(link.rx_refused)), (0, 0, 0), "no stale sends or refused input");
}

#[test]
fn blocked_data_and_summed_freed_precede_a_close() {
    let world = World::new(5, vec![
        Endpoint { responder: Responder::Rules(Rules::at_ready(vec![Then::message(MS, true, vec![1, 2, 3]), Then::message(MS, false, vec![4])]).into()) },
        Endpoint { responder: Responder::Sink },
    ], vec![]);
    let mut r = Rig::new(r#"
let b = bytes(4)
let a = net(net_tcp, 2, 3, fn() {})
let z = net(net_tcp, 2, 3, fn() {})
var first: i32 = 0
var second: i32 = 0
var took: i32 = 0
var invalid: i32 = 0
fn setup() {
    b[1] = 85
    b[2] = 86
    b[3] = 126
    net_open(a)
    net_open(z)
}
let work = after(ms(30), fn() {
    invalid = net_recv(a, b, net_state(a) + 1, 2) + net_send(z, b, 1, 0 - net_state(z), 0)
    for i in 0..800 { probe(1, i) }
    took = net_send(z, b, 1, 1, net_binary | net_more)
    took = took + net_send(z, b, 2, 1, net_more)
    took = took + net_send(z, b, 4, 0, 0)
    first = 100 * net_kind(a) + 10 * net_left(a)
    first = first + net_recv(a, b, 0, 4)
    took = took + net_send(a, b, 3, 1, 0)
    second = 100 * net_kind(a) + 10 * net_left(a)
    second = second + net_recv(a, b, 0, 4)
    net_close(z)
})
"#, world);
    r.until_pc(r.image.slot.symbols.dispatch); r.until_ms(20);
    r.machine.set_usb_host_listening(false); r.until_ms(50);
    assert_eq!((r.var("took"), r.var("first"), r.var("second"), r.var("invalid")), (3, 133, 11, 0),
        "ports returned while blocked, preserved the binary/text boundary and rejected both invalid ranges");
    assert!(r.word(r.image.slot.layout.lost_count) > 0, "the probe stream filled the physical ring");
    r.machine.set_usb_host_listening(true); r.until_ms(230);
    let sent: Vec<_> = r.link.up.iter().map(|(_, m)| m.clone()).collect();
    assert_eq!(&sent[..2], &[NetMsg::Open { c: 0 }, NetMsg::Open { c: 1 }]);
    let freed = sent.iter().position(|m| matches!(m, NetMsg::Freed { .. })).expect("Freed reached the host");
    let mut binary = Vec::new();
    for (i, msg) in sent[2..freed].iter().enumerate() {
        let NetMsg::Data { c: 1, flags, bytes } = msg else { panic!("expected binary Data, got {msg:?}") };
        let want = net::PART_BINARY | if i == 0 { net::PART_FIRST } else { 0 }
            | if 3 + i == freed { net::PART_LAST } else { 0 };
        assert_eq!(*flags, want); binary.extend_from_slice(bytes);
    }
    assert_eq!(binary, [85, 86], "the continuation kept the first send's kind and the zero-byte send ended it");
    assert_eq!(&sent[freed..], &[
        NetMsg::Freed { c: 0, slots: 2 },
        NetMsg::Data { c: 0, flags: net::PART_FIRST | net::PART_LAST, bytes: vec![126] },
        NetMsg::Close { c: 1 },
    ], "the summed slot credits, other sender and close survived backpressure in order");
    let faults: Vec<_> = r.link.faults.iter().map(|&(code, _, arg)| (code, arg)).collect();
    assert_eq!(faults, [(watch::FAULT_NET_RANGE, 3), (watch::FAULT_NET_RANGE, 1)]);
    assert_eq!(r.link.host.stale, 0);
}

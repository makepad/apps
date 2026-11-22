//! The networking scenarios on the reference interpreter: a client program (`support/net.splash`)
//! against the played network (`rt::net::World`) through the tether's host end (`rt::net::Host`),
//! every message between them through the shared codec. What is checked is what the program can
//! see (link and connection states, codes, epochs, message bytes and their order) and the times
//! the reference states, plus the flow control both ways: the device never gets a part it has no
//! slot for, and never sends past its grant.
use fab_elec_mcu::compile;
use fab_elec_mcu_rt::{module, net::{self, code, Condition, Endpoint, NetMsg, Outcome, Request, Responder, Rules, Then, UpstreamLog, World}, sched::{Device, Fault}};

#[path = "support/net_reference.rs"]
mod net_reference;
use net_reference::{run, run_on, EchoNet, Logged, Run, Wire, P};

const SOURCE: &str = include_str!("support/net.splash");
const MS: u64 = 1_000_000;
const S: u64 = 1_000_000_000;

impl Run {
    fn values(&self, id: u8) -> Vec<u16> { self.probes.iter().filter(|p| p.1 == id).map(|p| p.2).collect() }
    fn first(&self, id: u8, value: u16) -> u64 {
        self.probes.iter().find(|p| p.1 == id && p.2 == value).unwrap_or_else(|| panic!("probe {id} = {value}")).0
    }
    /// The state, code and detail the program saw at its first offline.
    fn first_end(&self) -> (u16, u16, u16) {
        let k = self.values(4).iter().position(|s| *s == u16::from(net::OFFLINE)).expect("an offline");
        (self.values(4)[k], self.values(5)[k], self.values(6)[k])
    }
    /// What the program saw, without the times.
    fn seen(&self) -> Vec<(u8, u16)> { self.probes.iter().map(|p| (p.1, p.2)).collect() }
    fn up(&self) -> impl Iterator<Item = &(u64, bool, NetMsg)> { self.wire.iter().filter(|w| w.1) }
    fn down(&self) -> impl Iterator<Item = &(u64, bool, NetMsg)> { self.wire.iter().filter(|w| !w.1) }
    /// The whole messages received: (size, wrong bytes, kind) from probes 9-12.
    fn messages(&self) -> Vec<(u32, u16, u16)> {
        let (lo, hi, bad, kind) = (self.values(9), self.values(10), self.values(11), self.values(12));
        (0..lo.len()).map(|k| (u32::from(lo[k]) | u32::from(hi[k]) << 16, bad[k], kind[k])).collect()
    }
    /// For each offline the host sent, how long until the device's next OPEN.
    fn backoffs(&self) -> Vec<u64> {
        let mut out = Vec::new();
        for (k, (t, up, msg)) in self.wire.iter().enumerate() {
            if !up && matches!(msg, NetMsg::State { state: net::OFFLINE, .. }) {
                if let Some((open, ..)) = self.wire[k + 1..].iter().find(|w| w.1 && matches!(w.2, NetMsg::Open { .. })) {
                    out.push(open - t);
                }
            }
        }
        out
    }
}

fn client(edits: &[(&str, &str)]) -> String {
    edits.iter().fold(SOURCE.to_string(), |source, (from, to)| {
        assert!(source.contains(from), "the client has `{from}`");
        source.replace(from, to)
    })
}

impl EchoNet {
    fn sent_bytes(&self) -> Vec<usize> {
        self.asked.iter().filter_map(|r| match r { Request::Send { bytes, .. } => Some(bytes.len()), _ => None }).collect()
    }
    fn opens(&self) -> usize { self.asked.iter().filter(|r| matches!(r, Request::Open { .. })).count() }
}

/// A time the reference states, give or take the handlers running at that instant.
fn near(t: u64, want: u64) -> bool { (want..want + MS).contains(&t) }

fn world(seed: u64, responder: Responder, conditions: Vec<(u64, Condition)>) -> World {
    World::new(seed, vec![Endpoint { responder }], conditions)
}

fn pattern(n: usize) -> Vec<u8> { (0..n).map(|j| ((j % 1500) * 7 + 3) as u8).collect() }

#[test]
fn a_large_message_crosses_both_ways_under_flow_control_and_replays_exactly() {
    const BIG: usize = 329_728;
    const RATE: u64 = 200_000;
    let source = client(&[("var goal: i32 = 0", "var goal: i32 = 329728")]);
    let conditions = vec![(0, Condition::Latency { c: 0, ns: 2 * MS, jitter_ns: MS }), (0, Condition::Rate { c: 0, bytes_per_s: RATE })];
    let r = run(&source, world(7, Responder::Echo, conditions.clone()), 10 * S);
    assert!(r.faults.is_empty() && r.stale == 0 && r.host_stale == 0, "nothing faults or crosses: {:?}", (&r.faults, r.stale, r.host_stale));
    assert_eq!((r.values(1), r.values(2)), (vec![2], vec![0]), "the link is up from the first handler on");
    assert_eq!(r.values(4), vec![1, 2], "connecting from the boot's open, then ready");
    assert_eq!(r.messages(), vec![(BIG as u32, 0, 1)], "the echo comes back whole, byte-exact and binary");
    assert!(r.values(13)[0] > 0, "the declared 4096-byte send queue filled: sends were told to try later");
    let up: Vec<&Vec<u8>> = r.up().filter_map(|w| match &w.2 { NetMsg::Data { bytes, .. } => Some(bytes), _ => None }).collect();
    assert_eq!(up.iter().map(|b| b.len()).sum::<usize>(), BIG, "every byte goes up once");
    assert_eq!(up.into_iter().flatten().copied().collect::<Vec<_>>(), pattern(BIG), "in order");
    let firsts = r.up().filter(|w| matches!(w.2, NetMsg::Data { flags, .. } if flags & net::PART_FIRST != 0)).count();
    let lasts = r.up().filter(|w| matches!(w.2, NetMsg::Data { flags, .. } if flags & net::PART_LAST != 0)).count();
    assert_eq!((firsts, lasts), (1, 1), "one message, sent in parts");
    let down = r.down().filter(|w| matches!(w.2, NetMsg::Data { .. })).count();
    assert_eq!(down, BIG.div_ceil(net::NET_PART), "the host splits the echo into full parts");
    // Each way the message crosses at RATE: the reference can be no faster.
    let done = r.first(9, (BIG & 0xffff) as u16);
    assert!(done >= 2 * BIG as u64 * S / RATE && done < 10 * S, "the echo is whole at {done} ns");

    assert_eq!(run(&source, world(7, Responder::Echo, conditions.clone()), 10 * S), r, "one seed replays every time and byte");
    let other = run(&source, world(8, Responder::Echo, conditions), 10 * S);
    assert_eq!(other.seen(), r.seen(), "another seed moves the times, not what the program sees");
    assert_ne!(other.probes, r.probes, "the seed drives the jitter");
}

#[test]
fn each_stage_reports_its_code_and_the_layer_backs_off() {
    let latency = (0, Condition::Latency { c: 0, ns: MS, jitter_ns: 0 });
    let window = |got: &[u64], want: &[u64], what: &str| {
        assert!(got.len() >= want.len(), "{what}: {got:?} has the retries {want:?}");
        for (g, w) in got.iter().zip(want) {
            assert!((w * S..w * S + MS).contains(g), "{what}: a retry {g} ns after the failure, {w} s expected: {got:?}");
        }
    };
    // Opens that fail at their stage: the first offline names it, and the layer keeps trying.
    for (outcome, expected, until) in [
        (Outcome::NotConfigured, (code::NOT_CONFIGURED, 0), 40 * S),
        (Outcome::NameNotFound, (code::NAME_NOT_FOUND, 3), 40 * S),
        (Outcome::Refused, (code::REFUSED, 0), 40 * S),
        (Outcome::Rejected(401), (code::REJECTED, 401), 40 * S),
        (Outcome::TimedOut, (code::TIMED_OUT, 0), 25 * S),
    ] {
        let r = run(SOURCE, world(1, Responder::Echo, vec![latency.clone(), (0, Condition::Open { c: 0, outcome })]), until);
        let what = format!("{outcome:?}");
        assert_eq!(r.first_end(), (0, u16::from(expected.0), expected.1), "{what}: the code and detail");
        if outcome == Outcome::NotConfigured {
            // Each attempt fails at the instant it starts, before the handler runs: state is read,
            // not queued, so the program sees offline throughout while the layer keeps trying.
            assert_eq!(r.values(4), [0], "{what}: offline throughout");
        } else {
            assert!(r.values(4).windows(2).all(|w| w[0] != w[1]) && r.values(4).len() >= 3, "{what}: offline and connecting again, by turns: {:?}", r.values(4));
        }
        assert!(!r.values(4).contains(&2), "{what}: never ready");
        assert!(r.values(7).is_empty(), "{what}: no ready period ended, so the epoch stays");
        if outcome == Outcome::TimedOut {
            assert!(near(r.first(4, 0), 10 * S), "{what}: the open gives up after its timeout");
            window(&r.backoffs(), &[1], &what);
        } else {
            window(&r.backoffs(), &[1, 2, 4, 8, 10, 10], &what);
        }
    }

    // The link's stages: what the program sees of the link, and the open waiting on it.
    let r = run(SOURCE, world(1, Responder::Echo, vec![latency.clone(), (0, Condition::LinkDown { code: code::NO_NETWORK, detail: 201 })]), 5 * S);
    assert_eq!((r.values(1), r.values(2), r.values(3)), (vec![0], vec![2], vec![201]), "no access point: link none, its code and Wi-Fi reason");
    assert_eq!(r.first_end(), (0, 2, 201), "the open fails with the link's code");
    let r = run(SOURCE, world(1, Responder::Echo, vec![latency.clone(), (0, Condition::Join { ns: 300 * MS, fails: Some((code::SIGN_IN_FAILED, 15)) })]), 5 * S);
    assert_eq!((r.values(1), r.values(2), r.values(3)), (vec![1, 0], vec![0, 3], vec![0, 15]), "joining, then sign-in failed with the handshake reason");
    assert_eq!(r.first_end(), (0, 3, 15), "the open waited for the join and failed with it");
    assert!(near(r.first(4, 0), 300 * MS), "at the join's end");
    assert_eq!(net::wifi_code(15), code::SIGN_IN_FAILED, "the played reason reads as the code the board would report");
    let r = run(SOURCE, world(1, Responder::Echo, vec![latency.clone(), (0, Condition::Join { ns: 300 * MS, fails: None })]), 5 * S);
    assert_eq!((r.values(1), r.values(4)), (vec![1, 2], vec![1, 2]), "joining then up; the open waits and is ready");
    assert!(near(r.first(4, 2), 302 * MS), "ready one round trip after the join");

    // Ends of a ready connection: the code, an epoch step, and a backoff reset by each ready.
    for (end, expected) in [
        (Condition::Disconnect { c: 0, code: code::RESET, detail: 0 }, (code::RESET, 0)),
        (Condition::Disconnect { c: 0, code: code::CLOSED, detail: 1001 }, (code::CLOSED, 1001)),
        (Condition::Disconnect { c: 0, code: code::PROTOCOL, detail: 0 }, (code::PROTOCOL, 0)),
    ] {
        let what = format!("{end:?}");
        let again = Condition::Disconnect { c: 0, code: code::RESET, detail: 0 };
        let r = run(SOURCE, world(1, Responder::Echo, vec![latency.clone(), (100 * MS, end), (3 * S, again)]), 6 * S);
        assert_eq!(r.values(4), vec![1, 2, 0, 1, 2, 0, 1, 2], "{what}: ready, ended, reopened, ready, twice");
        assert_eq!(r.first_end(), (0, u16::from(expected.0), expected.1), "{what}: the code and detail");
        assert_eq!(r.values(7), vec![1, 2], "{what}: each end steps the epoch");
        window(&r.backoffs(), &[1, 1], &what);
    }
    let r = run(SOURCE, world(1, Responder::Echo, vec![
        latency, (100 * MS, Condition::LinkDown { code: net::wifi_code(200), detail: 200 }), (2 * S, Condition::Join { ns: 50 * MS, fails: None }),
    ]), 6 * S);
    assert_eq!((r.values(1), r.values(2), r.values(3)), (vec![2, 0, 1, 2], vec![0, 4, 0, 0], vec![0, 200, 0, 0]), "a beacon timeout loses the link, which rejoins");
    assert_eq!((r.first_end(), r.values(7)), ((0, 4, 200), vec![1]), "the connection reads as a link loss");
    assert_eq!(r.values(4).last(), Some(&2), "and is ready again once the link is");
}

#[test]
fn a_loss_mid_message_steps_the_epoch_and_delivers_nothing_twice() {
    const SIZE: usize = 20_000;
    // A slow reader (1000 bytes every 10 ms) in two slots: the host holds the rest of the message.
    let source = client(&[("var slow: i32 = 0", "var slow: i32 = 1"), ("var step: i32 = 1600", "var step: i32 = 1000"),
        ("net(net_websocket, 4, 4096,", "net(net_websocket, 2, 4096,")]);
    let r = run(&source, world(3, Responder::Rules(Rules::at_ready(vec![Then::message(10 * MS, true, pattern(SIZE))]).into()), vec![
        (0, Condition::Latency { c: 0, ns: MS, jitter_ns: 0 }), (60 * MS, Condition::Disconnect { c: 0, code: code::RESET, detail: 0 }),
    ]), 3 * S);
    assert!(r.faults.is_empty() && r.host_stale == 0, "{:?}", (&r.faults, r.host_stale));
    let parts_before = r.down().filter(|w| w.0 < 60 * MS && matches!(w.2, NetMsg::Data { .. })).count();
    assert!(parts_before < SIZE.div_ceil(net::NET_PART), "the loss came mid-message: {parts_before} parts had gone");
    assert_eq!(r.values(7), vec![1], "the loss steps the epoch once");
    let dropped = r.values(8)[0];
    assert!(dropped > 0 && usize::from(dropped) < SIZE, "the program had read {dropped} bytes of the message the loss dropped");
    assert_eq!(r.messages(), vec![(SIZE as u32, 0, 1)], "the message arrives whole once, after the reconnect, never a remainder");
    assert!(r.first(9, SIZE as u16) > S, "the whole message is the next ready period's");
}

#[test]
fn close_ends_the_connection_and_a_reopen_waits_for_its_answer() {
    let echo = || world(5, Responder::Echo, vec![(0, Condition::Latency { c: 0, ns: MS, jitter_ns: 0 })]);
    let once = client(&[("var goal: i32 = 0", "var goal: i32 = 3000"), ("var close_after: i32 = 0", "var close_after: i32 = 1")]);
    let r = run(&once, echo(), 3 * S);
    assert_eq!(r.messages(), vec![(3000, 0, 1)], "one echo, then the close");
    assert_eq!((r.values(14), r.values(4)), (vec![1], vec![1, 2, 0]), "the close takes effect at once");
    assert_eq!((r.values(5)[2], r.values(7)), (0, vec![1]), "a close reads code 0 and ends the ready period");
    let close = r.wire.iter().position(|w| w.1 && matches!(w.2, NetMsg::Close { c: 0 })).expect("the device sends CLOSE");
    assert!(matches!(r.wire[close + 1].2, NetMsg::State { state: net::OFFLINE, code: code::NONE, .. }), "the host answers it offline");
    assert!(!r.wire[close..].iter().any(|w| w.1 && matches!(w.2, NetMsg::Open { .. })), "a closed connection is never reopened");

    let cycles = client(&[("var goal: i32 = 0", "var goal: i32 = 3000"), ("var close_after: i32 = 0", "var close_after: i32 = 1"), ("var reopen: i32 = 0", "var reopen: i32 = 1")]);
    let r = run(&cycles, echo(), S);
    let messages = r.messages();
    assert!(messages.len() >= 3 && messages.iter().all(|m| *m == (3000, 0, 1)), "each ready period carries its echo whole: {messages:?}");
    assert_eq!((r.values(14), r.values(15)), (vec![1; messages.len()], vec![1; messages.len()]), "every close and reopen is taken");
    for (k, w) in r.wire.iter().enumerate().filter(|(_, w)| w.1 && matches!(w.2, NetMsg::Close { .. })) {
        let open = k + 1 + r.wire[k + 1..].iter().position(|w| w.1 && matches!(w.2, NetMsg::Open { .. })).expect("a reopen follows");
        let answered = r.wire[k + 1..open].iter().any(|w| !w.1 && matches!(w.2, NetMsg::State { state: net::OFFLINE, .. }));
        assert!(answered, "the OPEN after the CLOSE at {} waits for the host's answer", w.0);
    }
    assert_eq!(r.stale + r.host_stale as usize, 0, "nothing crosses: one offline per open");
}

#[test]
fn net_ports_refuse_what_no_connection_has() {
    let refused = |source: &str, says: &str| {
        let errors = compile(source, &P, &[], &[]).err().unwrap_or_else(|| panic!("refused: {source}"));
        assert!(errors[0].message.contains(says), "{says}: {}", errors[0].message);
    };
    let handler = "fn() { probe(1, 1) }";
    refused("fn setup() { probe(1, net_link()) }", "`net_link` needs a net declaration");
    refused(&format!("let c = net(net_websocket, 1, 64, {handler})"), "at least two receive slots");
    refused(&format!("let c = net(2, 2, 64, {handler})"), "kind must be 0 (WebSocket) or 1 (TCP stream)");
    refused(&format!("let c = net(net_tcp, 2, 0, {handler})"), "send queue of at least one byte");
    refused(&format!("let c = net(net_tcp, 2, 64, {handler})\nfn setup() {{ net_open(1) }}"), "`net_open` names connection 1");

    // Computed values the loader cannot see fault at run time, and change nothing.
    let source = format!("let b = bytes(16)\nvar which: i32 = 3\nvar n: i32 = -1\nlet c = net(net_tcp, 2, 64, {handler})\n\
        fn setup() {{\n    probe(2, net_state(which))\n    probe(3, net_send(0, b, 0, n, 0))\n    probe(4, net_recv(0, b, 10, 7))\n    probe(5, net_open(0))\n}}");
    let c = compile(&source, &P, &[], &[]).unwrap_or_else(|e| panic!("{e:?}"));
    let mut dev = Device::new(module::load(&c.module, &P).expect("loads"), &P);
    let mut hal = Wire::default();
    dev.boot(&mut hal);
    let site = |f: &Fault| match *f { Fault::NetHandle { site, .. } | Fault::NetRange { site, .. } => site, _ => panic!("{f:?}") };
    assert!(matches!(hal.faults[..], [Fault::NetHandle { c: 3, .. }, Fault::NetRange { off: 0, .. }, Fault::NetRange { off: 10, .. }]), "{:?}", hal.faults);
    for (fault, call) in hal.faults.iter().zip(["net_state", "net_send", "net_recv"]) {
        let at = c.sites[site(fault) as usize];
        assert!(source[at.start as usize..at.end as usize].starts_with(call), "{call}: the fault names its call");
    }
    assert_eq!(hal.probes.iter().map(|p| p.2).collect::<Vec<_>>(), [0, 0, 0, 1], "faulting calls return 0; the open still goes");
    assert_eq!(hal.out.iter().map(|o| &o.1).collect::<Vec<_>>(), [&NetMsg::Open { c: 0 }], "and nothing else reached the host");
}

#[test]
fn a_real_server_through_the_log_replays_exactly_and_a_changed_rerun_reads_as_reset() {
    const BIG: usize = 100_000;
    let upstream = || World::new(9, vec![Endpoint { responder: Responder::Upstream }], vec![]);
    let sends = |goal: usize| client(&[("var goal: i32 = 0", &format!("var goal: i32 = {goal}"))]);
    let (big, small) = (sends(BIG), sends(2000));
    let mut log = UpstreamLog::new();
    let mut echo = EchoNet::default();
    // The stand-in answers at once, so an open is often ready before a handler reads it connecting.
    let settled = |r: &Run| r.values(4).into_iter().filter(|s| *s != u16::from(net::CONNECTING)).collect::<Vec<_>>();

    // Live: the message streams up in parts within the grants the network's takes allow, and the
    // echo comes back whole.
    let a = run_on(&big, upstream(), S, Some(Logged { log: &mut log, network: Some(&mut echo) }));
    assert!(a.faults.is_empty() && a.stale == 0 && a.host_stale == 0, "{:?}", (&a.faults, a.stale, a.host_stale));
    assert_eq!((settled(&a), a.messages()), (vec![2], vec![(BIG as u32, 0, 1)]), "ready, and the echo whole and byte-exact");
    assert_eq!((echo.opens(), echo.sent_bytes().iter().sum::<usize>()), (1, BIG), "the network saw one open and every byte once");
    let grants = a.down().filter(|w| matches!(w.2, NetMsg::Granted { .. })).count();
    assert!(grants > 1 && BIG > net::FIRST_GRANT as usize, "beyond the first grant, the network's takes topped it up: {grants} grants");
    let asked = echo.asked.len();

    // The run again from its start: everything from the log, nothing to the network.
    let b = run_on(&big, upstream(), S, Some(Logged { log: &mut log, network: Some(&mut echo) }));
    assert_eq!(b, a, "a rerun replays every time and byte");
    assert_eq!((echo.asked.len(), echo.closed_all, log.cuts), (asked, 0, 0), "and asks the network nothing");

    // A run that skips what its past asked (no message this time) meets the next answer unasked.
    let mut skipped_log = log.clone();
    let mut skipped_net = EchoNet { open: echo.open.clone(), ..EchoNet::default() };
    let s = run_on(&sends(0), upstream(), 3 * S, Some(Logged { log: &mut skipped_log, network: Some(&mut skipped_net) }));
    assert_eq!((skipped_log.cuts, skipped_net.closed_all), (1, 1), "the owner cuts its log and the network closes everything");
    assert_eq!(s.first_end(), (0, u16::from(code::RESET), 0), "the connection reads as reset");
    assert_eq!((skipped_net.opens(), skipped_net.sent_bytes().len(), settled(&s)), (1, 0, vec![2, 0, 2]), "the reopen goes out live and is ready");

    // A run that asks something else (a smaller message): cut, reset, the epoch steps, and the
    // reopen carries its message live, once.
    let c = run_on(&small, upstream(), 3 * S, Some(Logged { log: &mut log, network: Some(&mut echo) }));
    assert_eq!((log.cuts, echo.closed_all), (1, 1), "one cut");
    assert_eq!((settled(&c), c.values(7)), (vec![2, 0, 2], vec![1]), "ready, reset, ready again: the epoch steps once");
    assert_eq!(c.first_end(), (0, u16::from(code::RESET), 0), "the end reads as reset");
    assert_eq!(c.messages(), vec![(2000, 0, 1)], "the message the cut dropped is sent again and echoed once");
    assert_eq!((echo.opens(), echo.sent_bytes()[echo.sent_bytes().len() - 2..].iter().sum::<usize>()), (2, 2000),
        "the network saw the reopen and the new message, nothing of the cut attempt");
    let asked = echo.asked.len();

    // The changed run's own rerun replays its new past, cut and all.
    let d = run_on(&small, upstream(), 3 * S, Some(Logged { log: &mut log, network: Some(&mut echo) }));
    assert_eq!(d, c, "the new past replays exactly");
    assert_eq!((echo.asked.len(), log.cuts), (asked, 1), "with nothing asked and no new cut");

    // Another copy (no network) replays what the log has and never goes live: past the owner's time
    // its connection reads as reset, and the network and the log see nothing of it.
    let (logged_len, closed) = (log.len(), echo.closed_all);
    let e = run_on(&small, upstream(), 3 * S, Some(Logged { log: &mut log, network: None }));
    assert_eq!(e, c, "a copy within the owner's time replays it");
    let f = run_on(&small, upstream(), 5 * S, Some(Logged { log: &mut log, network: None }));
    let n = c.values(4).len();
    assert_eq!((&f.values(4)[..n], &f.values(5)[n..n + 1]), (&c.values(4)[..], &[u16::from(code::RESET)][..]), "the same past, then a reset where the owner's time ends");
    assert!(!f.values(4)[n..].contains(&2), "and never ready again: {:?}", f.values(4));
    assert_eq!((log.len(), log.cuts, echo.asked.len(), echo.closed_all), (logged_len, 1, asked, closed), "the copy changed nothing");
}

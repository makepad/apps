//! The networking vocabulary: the message codec both ends share, the Wi-Fi reason table, and the
//! host end's table-driven fake.
use fab_elec_mcu_rt::net::{self, code, Dir, NetMsg};
use std::sync::Arc;
use fab_elec_mcu_rt::watch;

#[test]
fn the_codec_round_trips_every_message_and_refuses_malformed_payloads() {
    let part = vec![0xa5; net::NET_PART];
    let messages = [
        (Dir::Up, NetMsg::Open { c: 3 }, vec![3]),
        (Dir::Up, NetMsg::Close { c: 255 }, vec![255]),
        (Dir::Up, NetMsg::Data { c: 1, flags: net::PART_FIRST | net::PART_BINARY, bytes: vec![9, 8, 7] }, vec![1, 3, 9, 8, 7]),
        (Dir::Down, NetMsg::Data { c: 0, flags: net::PART_LAST, bytes: part.clone() }, [&[0, 4][..], &part].concat()),
        (Dir::Up, NetMsg::Freed { c: 2, slots: 0x0102 }, vec![2, 2, 1]),
        (Dir::Down, NetMsg::Granted { c: 2, bytes: 0x0001_0000 }, vec![2, 0, 0, 1, 0]),
        (Dir::Down, NetMsg::State { c: 4, state: net::OFFLINE, code: code::REJECTED, detail: 401 }, vec![4, 0, 9, 0x91, 1, 0, 0]),
        (Dir::Down, NetMsg::Link { state: net::LINK_NONE, code: code::SIGN_IN_FAILED, detail: 15 }, vec![0, 3, 15, 0, 0, 0]),
    ];
    for (dir, msg, payload) in &messages {
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(&out, payload, "{msg:?}: the agreed payload layout");
        assert_eq!(NetMsg::decode(msg.kind(), *dir, &out).as_ref(), Some(msg), "{msg:?}: round trip");
        assert!(watch::NET_OPEN <= msg.kind() && msg.kind() <= watch::NET_LINK, "{msg:?}: a net kind");
        if !matches!(msg, NetMsg::Data { .. }) {
            assert_eq!(NetMsg::decode(msg.kind(), *dir, &out[..out.len() - 1]), None, "{msg:?}: one byte short");
            out.push(0);
            assert_eq!(NetMsg::decode(msg.kind(), *dir, &out), None, "{msg:?}: one byte long");
        }
    }
    assert_eq!(watch::PROTO, 4, "the net kinds are protocol 4");
    let refused: [(u8, Dir, Vec<u8>, &str); 9] = [
        (watch::NET_OPEN, Dir::Down, vec![0], "OPEN only goes up"),
        (watch::NET_STATE, Dir::Up, vec![0, 2, 0, 0, 0, 0, 0], "STATE only comes down"),
        (watch::NET_DATA, Dir::Up, vec![0, net::PART_FIRST], "an empty part"),
        (watch::NET_DATA, Dir::Up, [&[0, 0][..], &vec![0; net::NET_PART + 1]].concat(), "a part past NET_PART"),
        (watch::NET_DATA, Dir::Down, vec![0, 8, 1], "an unknown part flag"),
        (watch::NET_CREDIT, Dir::Up, vec![0, 0, 0], "zero slots freed"),
        (watch::NET_CREDIT, Dir::Down, vec![0, 0, 0, 0, 0], "zero bytes granted"),
        (watch::NET_STATE, Dir::Down, vec![0, 3, 0, 0, 0, 0, 0], "a state above ready"),
        (watch::NET_LINK, Dir::Down, vec![2, code::LAST + 1, 0, 0, 0, 0], "a code past the table"),
    ];
    for (kind, dir, payload, why) in refused {
        assert_eq!(NetMsg::decode(kind, dir, &payload), None, "{why}");
    }
}

#[test]
fn wifi_reasons_map_to_the_agreed_codes() {
    // The pinned enum has 60 values; each has one row, in value order.
    assert_eq!(net::WIFI_REASONS.len(), 60);
    assert!(net::WIFI_REASONS.windows(2).all(|w| w[0].0 < w[1].0), "rows in value order, no repeats");
    let no_network = [5, 30, 201, 203, 205, 208, 210, 211, 212];
    let sign_in = [14, 15, 17, 18, 19, 20, 21, 22, 23, 24, 29, 49, 202, 204];
    for &(reason, name, code) in net::WIFI_REASONS {
        let expected = if no_network.contains(&reason) { code::NO_NETWORK } else if sign_in.contains(&reason) { code::SIGN_IN_FAILED } else { code::LINK_LOST };
        assert_eq!(code, expected, "reason {reason} {name}");
        assert_eq!(net::wifi_code(reason), expected, "reason {reason} {name} by lookup");
        assert_eq!(net::wifi_reason(reason), Some((name, code)));
    }
    for unknown in [0, 40, 45, 52, 66, 69, 199, 213, u16::MAX] {
        assert_eq!(net::wifi_reason(unknown), None, "{unknown} is not in the pinned enum");
        assert_eq!(net::wifi_code(unknown), code::LINK_LOST, "an unknown reason reads as a link loss");
    }
    for code in 0..=code::LAST {
        assert_ne!(net::code_text(code), "unknown", "code {code} has its text");
    }
}

const MS: u64 = 1_000_000;

/// A device that reads every part at once on connection 0, which it opens at 0: it frees each slot
/// as a part arrives and sends each text of `says` whole at its time. The whole messages it takes
/// in `from..until`: (time, binary, bytes).
fn drive(host: &mut net::Host, from: u64, until: u64, says: &[(u64, &str)]) -> Vec<(u64, bool, Vec<u8>)> {
    let (mut got, mut partial) = (Vec::new(), Vec::new());
    let mut says = says.iter().filter(|(t, _)| (from..until).contains(t)).peekable();
    loop {
        let say = says.peek().map(|(t, _)| *t);
        let Some(now) = [host.next_due(), say].into_iter().flatten().min().filter(|t| *t < until) else { return got };
        while let Some((t, msg)) = host.poll(now) {
            if let NetMsg::Data { c, flags, bytes } = msg {
                partial.extend_from_slice(&bytes);
                host.from_device(t, &NetMsg::Freed { c, slots: 1 }).unwrap();
                if flags & net::PART_LAST != 0 { got.push((t, flags & net::PART_BINARY != 0, std::mem::take(&mut partial))); }
            }
        }
        if say == Some(now) {
            let bytes = says.next().unwrap().1.as_bytes().to_vec();
            host.from_device(now, &NetMsg::Data { c: 0, flags: net::PART_FIRST | net::PART_LAST, bytes }).unwrap();
        }
    }
}

#[test]
fn rules_answer_by_their_table_replay_from_a_mark_and_share_their_bytes() {
    use net::{Field, Payload, Responder, Rule, Rules, Then, When};
    let text = |s: &str| Field::Bytes(s.as_bytes().to_vec());
    let asked = |verb: &str, whole| When::Message { binary: false, bytes: verb.as_bytes().to_vec(), whole };
    let done = || Then { head: vec![text("done "), Field::Word(1), text(" "), Field::Sent], ..Then::message(0, false, Vec::new()) };
    let speech: Payload = (0..6400u32).map(|i| (i * 7) as u8).collect::<Vec<u8>>().into();
    let picture: Payload = vec![0x5a; 330_000].into();
    let rules = Rules(vec![
        Rule { when: When::Ready, stop: false, then: vec![Then::message(0, false, b"hi".to_vec())] },
        Rule { when: asked("ask ", false), stop: false, then: vec![
            Then { head: vec![text("got "), Field::Word(1)], ..Then::message(2 * MS, false, Vec::new()) },
            Then { head: vec![Field::Bytes(vec![b'A', 0]), Field::Word16(1), Field::Offset32], block: 640, every_ns: 20 * MS, unit: 2,
                ..Then::message(MS, true, speech.clone()) },
            done(),
        ] },
        Rule { when: asked("stop ", false), stop: true, then: vec![done()] },
        Rule { when: asked("pictures", true), stop: false, then: (0..10).map(|_| Then::message(0, true, picture.clone())).collect() },
    ]);
    let world = net::World::new(11, vec![net::Endpoint { responder: Responder::Rules(rules.into()) }],
        vec![(0, net::Condition::Latency { c: 0, ns: MS, jitter_ns: 300_000 })]);
    let mut host = net::Host::new(world, &[4]);
    host.hello(0);
    host.from_device(0, &NetMsg::Open { c: 0 }).unwrap();
    let says = [(10 * MS, "ask 7"), (400 * MS, "ask 9"), (450 * MS, "stop 9"), (600 * MS, "stop 9"), (700 * MS, "pictures")];
    let mut all = drive(&mut host, 0, 420 * MS, &says);
    let mark = host.clone();
    let mut rest = drive(&mut host, 420 * MS, 701_600_000, &says);
    let before = Arc::strong_count(&picture);
    let held = host.clone();
    assert_eq!(Arc::strong_count(&picture) - before, 10, "a mark shares the ten pictures in flight, never copies them");
    drop(held);
    rest.extend(drive(&mut host, 701_600_000, 1000 * MS, &says));
    assert_eq!(drive(&mut mark.clone(), 420 * MS, 1000 * MS, &says), rest, "a rerun from the mark gives the same bytes at the same times");
    all.extend(rest);

    let texts: Vec<_> = all.iter().filter(|m| !m.1).map(|m| String::from_utf8(m.2.clone()).unwrap()).collect();
    let blocks = |turn: u16| -> Vec<(usize, u32)> {
        all.iter().enumerate().filter(|(_, m)| m.1 && m.2[..4] == [b'A', 0, turn as u8, (turn >> 8) as u8]).map(|(k, m)| {
            let offset = u32::from_le_bytes(m.2[4..8].try_into().unwrap());
            assert_eq!(m.2[8..], speech[offset as usize * 2..][..640], "block {offset} of turn {turn} carries its part of the body");
            (k, offset)
        }).collect()
    };
    let (seven, nine) = (blocks(7), blocks(9));
    assert_eq!(seven.iter().map(|b| b.1).collect::<Vec<_>>(), (0..10).map(|k| k * 320).collect::<Vec<_>>(), "the whole body in blocks, offsets in units");
    assert!((1..10).contains(&nine.len()) && nine.iter().enumerate().all(|(k, b)| b.1 == k as u32 * 320), "the stop came mid-body: {nine:?}");
    let stopped = format!("done 9 {}", 320 * nine.len());
    assert_eq!(texts, ["hi", "got 7", "done 7 3200", "got 9", stopped.as_str()],
        "the ready rule, the turn's words filled in, the units sent; a stop answers once with what was sent and, finding nothing, not at all");
    let done9 = all.iter().position(|m| m.2 == stopped.as_bytes()).unwrap();
    assert!(nine.iter().all(|b| b.0 < done9), "nothing of a stopped send follows its answer");
    let pictures: Vec<_> = all.iter().filter(|m| m.1 && m.2.len() == picture.len()).collect();
    assert!(pictures.len() == 10 && pictures.iter().all(|m| m.2[..] == picture[..]), "ten whole pictures");
}

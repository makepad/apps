use fab_elec_mcu_netgw::{base64, sha1, websocket_accept, HostEvent, Message, Tether, TetherStats};
use fab_elec_mcu_netgw::wire::checksum;
use fab_elec_mcu_rt::watch::{self, Event};
mod support;
use support::{Client, Device, Rig};
use support::wire::*;

#[test]
fn virtual_network_and_messages_survive_seeded_faults() {
    for seed in [None, Some(1), Some(0x1020_3040), Some(0xfeed_beef)] {
        let mut r = Rig::new(seed);
        let offer = r.exchange(&dhcp(1, None, [0; 4]), |f| udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[2])));
        let offer = udp_payload(&offer, 67).unwrap();
        assert_eq!(&offer[16..20], &DEVICE_IP, "DISCOVER offers the configured address, seed {seed:?}");
        let ack = r.exchange(&dhcp(3, Some(DEVICE_IP), [0; 4]), |f| udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[5])));
        let ack = udp_payload(&ack, 67).unwrap();
        assert_eq!(&ack[16..20], &DEVICE_IP, "REQUEST acknowledges the offered address");
        assert_eq!(dhcp_option(ack, 3), Some(GATEWAY_IP.as_slice()), "DHCP router is the gateway");
        assert_eq!(dhcp_option(ack, 6), Some(GATEWAY_IP.as_slice()), "DHCP DNS server is the gateway");
        assert_eq!(dhcp_option(ack, 1), Some([255, 255, 255, 0].as_slice()), "DHCP subnet mask follows prefix");
        assert_eq!(dhcp_option(ack, 51), Some(3600u32.to_be_bytes().as_slice()), "DHCP supplies lease duration");
        assert_eq!(dhcp_option(ack, 58), Some(1800u32.to_be_bytes().as_slice()), "DHCP T1 is half the lease");
        assert_eq!(dhcp_option(ack, 59), Some(3150u32.to_be_bytes().as_slice()), "DHCP T2 is seven eighths of lease");

        let resolved = r.exchange(&arp(), |f| f.len() >= 42 && read16(f, 12) == 0x0806);
        assert_eq!(read16(&resolved, 20), 2, "ARP response is a reply");
        assert_eq!(&resolved[22..28], &GATEWAY_MAC, "ARP resolves the gateway MAC");
        assert_eq!(r.gateway.learned_device_mac(), Some(DEVICE_MAC), "gateway learns device source MAC");
        let pong = r.exchange(&ping(), |f| payload(f, 1).is_some_and(|p| p[0] == 0));
        let pong = payload(&pong, 1).unwrap();
        assert_eq!(checksum(pong), 0, "echo reply checksum is valid");
        assert_eq!(&pong[4..], &[0, 7, 0, 1, 9, 8, 7], "echo preserves identifier, sequence and payload");

        let answer = r.exchange(&dns("BrIdGe.InVaLiD", 1), |f| udp_payload(f, 53).is_some_and(|d| read16(d, 6) == 1));
        let answer = udp_payload(&answer, 53).unwrap();
        assert_eq!(&answer[answer.len() - 4..], &ENDPOINT_IP, "DNS A lookup is case insensitive");
        assert_eq!(read32(answer, answer.len() - 10), 60, "DNS A response TTL is 60 seconds");
        assert_eq!(read16(answer, 2) & 0x80, 0, "DNS never advertises recursion");
        let missing = r.exchange(&dns("absent.invalid", 1), |f| udp_payload(f, 53).is_some_and(|d| read16(d, 2) & 15 == 3));
        assert_eq!(read16(udp_payload(&missing, 53).unwrap(), 2) & 15, 3, "unknown DNS name returns NXDOMAIN");

        let (index, conn) = r.connect();
        let mut expected = Vec::new();
        for size in [1usize, 125, 126, 65535, 65536, 330_000] {
            let data: Vec<u8> = (0..size).map(|i| (i.wrapping_mul(37) ^ size) as u8).collect();
            r.clients[index].queue(ws(2, true, true, &data));
            expected.push(Message::Binary(data));
            let count = expected.len();
            r.until("binary message is echoed without loss", |r| r.clients[index].messages.len() >= count);
            assert_eq!(r.clients[index].messages, expected, "binary echo is byte-exact and ordered at size {size}, seed {seed:?}");
        }
        let text = "microsplash — voice 🎙";
        r.clients[index].queue(ws(1, false, true, &text.as_bytes()[..14]));
        r.clients[index].queue(ws(9, true, true, b"ping"));
        r.clients[index].queue(ws(0, true, true, &text.as_bytes()[14..]));
        expected.push(Message::Text(text.into()));
        r.until("fragmented UTF-8 and interleaved ping complete", |r| r.clients[index].messages.len() == 7 && !r.clients[index].pongs.is_empty());
        assert_eq!(r.clients[index].messages, expected, "text continuation preserves UTF-8 across frame boundaries");
        assert_eq!(r.clients[index].pongs, vec![b"ping".to_vec()], "ping receives one matching pong");
        let delivered: Vec<_> = r.events.iter().filter_map(|e| match e {
            HostEvent::Message { conn: id, msg } if *id == conn => Some(msg.clone()), _ => None,
        }).collect();
        assert_eq!(delivered, expected, "host sees every message exactly once and in order");
        r.until("host pacing reaches zero after ACKs", |r| r.gateway.host_pending(conn) == 0);

        r.clients[index].close_device();
        r.until("device initiated close finishes TCP", |r| r.clients[index].peer_fin && r.clients[index].fin_acked);
        assert_eq!(r.clients[index].close.as_ref().map(|v| v.0), Some(1000), "device close is echoed");
        assert!(r.events.iter().any(|e| matches!(e, HostEvent::Closed { conn: id, code: 1000, .. } if *id == conn)), "device close reaches host");
        let (second, second_conn) = r.connect();
        r.gateway.host_close(r.now, second_conn, 1000, "complete");
        r.until("host initiated close finishes TCP", |r| r.clients[second].peer_fin && r.clients[second].fin_acked);
        assert_eq!(r.clients[second].close, Some((1000, "complete".into())), "host close reason reaches device");
        assert!(r.events.iter().any(|e| matches!(e, HostEvent::Closed { conn: id, code: 1000, reason } if *id == second_conn && reason == "complete")), "device echoes the host close");
        r.for_ms(1100);
        let stats = r.gateway.stats();
        assert_eq!(stats.ws_messages_in, 7, "no device messages lost or duplicated");
        assert_eq!(stats.ws_messages_out, 7, "host echoes every delivered message");
        let total: usize = expected.iter().map(Message::len).sum();
        assert_eq!(stats.ws_bytes_in, total as u64, "device byte accounting has no loss");
        assert_eq!(stats.ws_bytes_out, total as u64, "host byte accounting has no loss");
        assert_eq!(stats.ws_closed, 2, "each connection emits Closed once");
        assert_eq!(stats.ws_protocol + stats.ws_utf8 + stats.tcp_checksum, 0, "faults preserve valid protocol bytes");
        if seed.is_some() {
            for faults in [stats.faults_in, stats.faults_out] {
                assert!(faults.dropped > 0 && faults.duplicated > 0 && faults.reordered > 0 && faults.delayed > 0,
                    "all faults are injected in both directions, seed {seed:?}: {faults:?}");
            }
            assert!(stats.tcp_out_of_order > 0, "faulty link exercises gateway reassembly");
            assert!(stats.tcp_retransmits > 0, "faulty link exercises gateway retransmission");
            assert!(r.clients.iter().any(|c| c.retransmits > 0), "faulty link exercises independent client retransmission");
        } else {
            assert_eq!(stats.faults_in, Default::default(), "faultless input stays faultless");
            assert_eq!(stats.faults_out, Default::default(), "faultless output stays faultless");
        }
    }
}

#[test]
fn refusals_and_malformed_inputs_have_distinct_counters() {
    let mut r = Rig::new(None);
    let refused = r.exchange(&tcp(39000, 9, 77, 0, 2, 65535, &[]), |f| payload(f, 6).is_some());
    let refused = payload(&refused, 6).unwrap();
    assert_eq!(refused[13], 20, "non-endpoint SYN gets RST ACK");
    assert_eq!(read32(refused, 8), 78, "refusal acknowledges SYN sequence space");
    assert_eq!(r.gateway.stats().tcp_refused, 1, "endpoint refusal has its own counter");
    let nak = r.exchange(&dhcp(3, Some([192, 0, 2, 99]), [0; 4]), |f| udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[6])));
    assert_eq!(dhcp_option(udp_payload(&nak, 67).unwrap(), 53), Some([6].as_slice()), "wrong DHCP address receives NAK");
    assert_eq!(r.gateway.stats().dhcp_naks, 1, "DHCP refusal has its own counter");

    let mut fragment = ping();
    write16(&mut fragment, 20, 0x2000);
    write16(&mut fragment, 24, 0);
    let check = checksum(&fragment[14..34]);
    write16(&mut fragment, 24, check);
    r.gateway.frame_in(r.now, &fragment);
    let mut corrupt = ping();
    corrupt[24] ^= 1;
    r.gateway.frame_in(r.now, &corrupt);
    r.gateway.frame_in(r.now, &udp(DEVICE_IP, GATEWAY_IP, (53000, 53), &[0, 1, 2]));
    assert!(r.gateway.frames_out(r.now).is_empty(), "malformed network packets produce no responses");
    let stats = r.gateway.stats();
    assert_eq!(stats.ip_fragment, 1, "IP fragments have their own drop counter");
    assert_eq!(stats.ip_checksum, 1, "bad IP checksum has its own drop counter");
    assert_eq!(stats.dns_malformed, 1, "malformed DNS has its own drop counter");

    let (index, _) = r.connect();
    r.clients[index].queue(ws(2, true, false, b"unmasked"));
    r.until("unmasked WebSocket is refused", |r| r.clients[index].close.is_some());
    assert_eq!(r.clients[index].close.as_ref().map(|v| v.0), Some(1002), "unmasked client frame receives protocol-error close");
    assert_eq!(r.gateway.stats().ws_unmasked, 1, "unmasked client frame has its own counter");
}

#[test]
fn window_pacing_reassembly_and_rewind() {
    let mut r = Rig::new(None);
    let (index, conn) = r.connect();
    r.until("upgrade response is acknowledged", |r| r.gateway.host_pending(conn) == 0 && r.clients[index].idle());
    r.echo = false;
    r.drain_host = false;
    let data = vec![0x5a; 70_000];
    r.clients[index].queue(ws(2, true, true, &data));
    // Deliver the first flight backwards. A second independent client is not
    // involved: these remain the active peer's actual sequence numbers.
    let first = r.clients[index].poll(r.now);
    for packet in first.into_iter().rev() { r.gateway.frame_in(r.now, &packet); }
    r.until("undrained complete message closes receive window", |r| r.clients[index].peer_window == 0);
    assert!(r.gateway.stats().tcp_out_of_order > 0, "gateway buffers reversed TCP segments");
    assert_eq!(r.gateway.stats().ws_messages_in, 1, "large message completes before host drains events");
    assert!(r.clients[index].zero_windows > 0, "slow host is observable as TCP back-pressure");
    r.drain_host = true;
    r.until("host drain reopens receive window", |r| r.clients[index].peer_window > 0 && r.clients[index].idle());
    assert!(r.events.iter().any(|event| matches!(event, HostEvent::Message { msg: Message::Binary(got), .. } if got == &data)),
        "out-of-order reassembly preserves the whole message");

    r.clients[index].advertise(0);
    r.pump();
    r.gateway.host_send(r.now, conn, Message::Binary(vec![9; 4096]));
    let pending = r.gateway.host_pending(conn);
    assert!(pending >= 4096, "host_pending counts bytes retained while device window is zero");
    r.for_ms(650);
    assert!(r.gateway.stats().tcp_probes > 0, "gateway probes a zero device window on a timer");
    assert_eq!(r.gateway.host_pending(conn), pending, "unacknowledged probe retains host data");
    assert!(r.clients[index].messages.is_empty(), "closed receive window does not consume probe data");
    r.clients[index].advertise(65535);
    r.until("window reopening delivers queued host data", |r| !r.clients[index].messages.is_empty() && r.gateway.host_pending(conn) == 0);
    assert_eq!(r.clients[index].messages, vec![Message::Binary(vec![9; 4096])], "zero-window probe cannot duplicate stream bytes");

    let before = r.gateway.stats().tcp_retransmits;
    r.gateway.host_send(r.now, conn, Message::Binary(vec![3; 30]));
    let lost = r.gateway.frames_out(r.now);
    assert!(!lost.is_empty(), "timer test withholds a real outbound segment");
    r.until("RTO repairs a lost segment", |r| r.clients[index].messages.len() == 2 && r.gateway.host_pending(conn) == 0);
    assert!(r.gateway.stats().tcp_retransmits > before, "lost segment increments retransmission counter");
    assert_eq!(r.clients[index].messages[1], Message::Binary(vec![3; 30]), "RTO retransmission preserves payload");

    r.gateway.host_send(r.now, conn, Message::Binary(vec![4; 24_000]));
    r.until("congestion window grows after recovery", |r| r.clients[index].messages.len() == 3 && r.gateway.host_pending(conn) == 0);
    let fast_before = r.gateway.stats().tcp_fast_retransmits;
    r.gateway.host_send(r.now, conn, Message::Binary(vec![5; 24_000]));
    let flight = r.gateway.frames_out(r.now);
    assert!(flight.len() >= 4, "recovered connection can send enough segments for duplicate ACKs");
    for frame in flight.into_iter().skip(1) {
        r.clients[index].receive(payload(&frame, 6).unwrap());
        for ack in r.clients[index].poll(r.now) { r.gateway.frame_in(r.now, &ack); }
    }
    assert!(r.gateway.stats().tcp_fast_retransmits > fast_before, "three duplicate ACKs retransmit without advancing time");
    r.until("fast retransmit repairs missing prefix", |r| r.clients[index].messages.len() == 4 && r.gateway.host_pending(conn) == 0);
    assert_eq!(r.clients[index].messages[3], Message::Binary(vec![5; 24_000]), "fast recovery keeps reassembled bytes intact");

    let mut restored = r.gateway.clone();
    for gateway in [&mut r.gateway, &mut restored] { gateway.host_send(r.now, conn, Message::Text("rewind".into())); }
    let original_frames = r.gateway.frames_out(r.now);
    assert_eq!(original_frames, restored.frames_out(r.now), "cloned TCP congestion, sequence and WebSocket state replay identically");
    assert_eq!(r.gateway.next_deadline(), restored.next_deadline(), "clone retains active retransmission deadline");
    for frame in original_frames { r.clients[index].receive(payload(&frame, 6).unwrap()); }
    r.until("snapshot comparison delivers its original traffic", |r| r.clients[index].messages.len() == 5 && r.gateway.host_pending(conn) == 0);

    let mut a = Rig::new(Some(42)).gateway;
    a.frame_in(0, &arp());
    let mut b = a.clone();
    for now in [1, 10, 50, 250, 500] {
        a.frame_in(now, &ping());
        b.frame_in(now, &ping());
        assert_eq!(a.frames_out(now), b.frames_out(now), "cloned fault queues and PRNG replay identical frames");
        assert_eq!(a.next_deadline(), b.next_deadline(), "cloned gateway preserves timer deadlines");
        assert_eq!(a.stats(), b.stats(), "cloned gateway preserves counters");
    }
}

#[test]
fn slow_host_reader_holds_the_window_without_loss() {
    let mut r = Rig::new(Some(0x51_0e));
    let (index, conn) = r.connect();
    r.echo = false;
    let sizes = [100_000usize, 1, 70_000, 5, 120_000, 64_000];
    let expected: Vec<_> = sizes.iter().enumerate()
        .map(|(n, &size)| Message::Binary((0..size).map(|i| (i * 31 + n) as u8).collect())).collect();
    for msg in &expected {
        let Message::Binary(data) = msg else { unreachable!() };
        r.clients[index].queue(ws(2, true, true, data));
    }
    let delivered = |r: &Rig| -> Vec<Message> {
        r.events.iter().filter_map(|e| match e {
            HostEvent::Message { conn: id, msg } if *id == conn => Some(msg.clone()), _ => None,
        }).collect()
    };
    // The device queues everything at once; the host reads once every 50 ms.
    let mut rounds = 0;
    while delivered(&r).len() < expected.len() {
        r.drain_host = false;
        r.for_ms(50);
        r.drain_host = true;
        r.pump();
        rounds += 1;
        assert!(rounds < 4000, "a slow reader still receives every message; stats {:?}", r.gateway.stats());
    }
    assert_eq!(delivered(&r), expected, "every message reaches the slow host whole and in order");
    assert!(r.clients[index].zero_windows > 0, "a full receive window holds the device back (zero window)");
    let stats = r.gateway.stats();
    let total: usize = sizes.iter().sum();
    assert_eq!((stats.ws_messages_in, stats.ws_bytes_in), (sizes.len() as u64, total as u64), "nothing dropped or duplicated");
    assert_eq!((stats.ws_protocol, stats.ws_closed), (0, 0), "back-pressure never fails or closes the connection");
    r.until("the drained window reopens fully", |r| r.clients[index].idle() && r.clients[index].peer_window == 65535);
}

#[test]
fn device_reset_closes_connections_and_starts_clean() {
    let mut r = Rig::new(None);
    let (_, conn) = r.connect();
    r.echo = false;
    r.gateway.host_send(r.now, conn, Message::Binary(vec![1; 3000]));
    r.gateway.device_reset(r.now);
    assert!(r.gateway.frames_out(r.now).is_empty(), "frames for the old device are dropped as stale");
    let closed = r.gateway.host_events(r.now).into_iter().filter(|e| matches!(e,
        HostEvent::Closed { conn: id, code: 1006, reason } if *id == conn && reason == "device reset")).count();
    assert_eq!(closed, 1, "the host hears the open connection close once, with code 1006");
    assert_eq!(r.gateway.host_pending(conn), 0, "the old connection holds no host data");
    assert_eq!(r.gateway.learned_device_mac(), None, "the learned MAC is forgotten");
    assert_eq!(r.gateway.next_deadline(), None, "no timer of the old device survives the reset");
    r.gateway.host_send(r.now, conn, Message::Text("late".into()));
    let stats = r.gateway.stats();
    assert_eq!((stats.device_resets, stats.host_unknown_conn), (1, 1), "the reset is counted and a late send is refused");
    // The restarted device begins again: DHCP, then a new connection on the same ports.
    r.clients.clear();
    r.events.clear();
    let ack = r.exchange(&dhcp(3, Some(DEVICE_IP), [0; 4]), |f| udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[5])));
    assert_eq!(&udp_payload(&ack, 67).unwrap()[16..20], &DEVICE_IP, "the restarted device leases its address again");
    let (_, fresh) = r.connect();
    assert_ne!(fresh, conn, "the restarted device gets a new connection");
}

#[test]
fn tether_credits_hello_and_order() {
    let mut gw = Rig::new(None).gateway;
    let mut tether = Tether::new();
    let mut sent = Vec::new();
    assert_eq!(tether.stats(), TetherStats::default(), "new tether is down, empty and has no credit or history");

    for length in [0, 13, 1515] { tether.eth_in(&mut gw, 0, &vec![0; length]); }
    assert_eq!(tether.stats().bad_lengths, 3, "each invalid FC1 length is counted separately from gateway refusals");
    assert_eq!(tether.stats().frames_in, 3, "input accounting includes rejected ETH frames");
    assert_eq!(gw.stats().frames_in, 0, "bad FC1 lengths never reach the gateway");
    assert_eq!(gw.stats().ethernet_short, 0, "short frames are rejected at the tether boundary");
    for length in [14, 1514] {
        tether.eth_in(&mut gw, 0, &ethernet(0x88b5, &vec![0; length - 14], false));
    }
    assert_eq!((gw.stats().frames_in, gw.stats().ethernet_type), (2, 2), "both inclusive FC1 length boundaries reach the gateway");
    assert_eq!(tether.stats().bad_lengths, 3, "valid boundary lengths do not increment bad_lengths");

    for request in [arp(), ping(), dns("bridge.invalid", 1), dhcp(1, None, [0; 4])] {
        tether.eth_in(&mut gw, 0, &request);
    }
    // Snapshot the gateway output before Tether takes it, so the comparison
    // checks the full Ethernet bytes and the gateway's original ordering.
    let mut expected = gw.clone().frames_out(0);
    assert_eq!(expected.len(), 4, "four distinct requests produce four replies for the ordering check");
    tether.poll(&mut gw, 0, &mut |frame| sent.push(frame.to_vec()));
    assert!(sent.is_empty(), "nothing is sent before the first credit");
    assert_eq!((tether.stats().queue_len, tether.stats().queue_high_water), (4, 4), "down-link output queues and sets the high-water mark");
    assert!(!tether.stats().link_up, "incoming ETH alone does not bring the link up");

    tether.credit(0, 2);
    tether.poll(&mut gw, 0, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!(sent, expected[..2], "two credits send exactly the two oldest frames, unchanged and in order");
    assert_eq!((tether.stats().credits, tether.stats().queue_len), (0, 2), "one credit is spent per sent frame");
    assert!(tether.stats().link_up, "the first credit brings the link up and exhaustion leaves it up");

    tether.eth_in(&mut gw, 0, &dhcp(3, Some(DEVICE_IP), [0; 4]));
    expected.extend(gw.clone().frames_out(0));
    tether.poll(&mut gw, 0, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!(sent.len(), 2, "an up link with exhausted credit sends nothing further");
    assert_eq!(tether.stats().queue_len, 3, "new replies join the remaining FIFO even without credit");
    tether.credit(0, 1);
    tether.poll(&mut gw, 0, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!(sent, expected[..3], "a later reply cannot overtake a queued reply");
    tether.credit(0, 2);
    tether.poll(&mut gw, 0, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!(sent, expected, "further credit drains every remaining frame in order");
    assert_eq!((tether.stats().queue_len, tether.stats().frames_sent, tether.stats().credits_received), (0, 5, 5), "drain accounting counts frames and granted buffers");

    for request in [arp(), ping()] { tether.eth_in(&mut gw, 1, &request); }
    tether.poll(&mut gw, 1, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!(tether.stats().queue_len, 2, "reset fixture has frames waiting in the tether");
    tether.credit(1, 7);
    // Leave another reply inside Gateway; device_reset must clear that too.
    tether.eth_in(&mut gw, 1, &dhcp(1, None, [0; 4]));
    tether.hello(&mut gw, 1);
    let reset = tether.stats();
    assert_eq!((reset.credits, reset.link_up, reset.queue_len), (0, false, 0), "Hello forgets credit, drops the link and empties the FIFO");
    assert_eq!((reset.hellos, reset.frames_discarded_on_hello), (1, 2), "Hello counts exactly the frames it discarded from the tether");
    assert_eq!((reset.frames_sent, reset.credits_received, reset.queue_high_water), (5, 12, 4), "Hello preserves lifetime counters and high-water history");
    assert_eq!(gw.stats().device_resets, 1, "Hello calls Gateway::device_reset once");
    assert_eq!(gw.learned_device_mac(), None, "Hello also clears gateway device state");
    assert_eq!(gw.next_deadline(), None, "Hello removes the old device's DHCP lease timer");
    tether.poll(&mut gw, 1, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!(tether.stats().queue_len, 0, "gateway output pending at Hello cannot reappear in the tether");
    assert_eq!(sent, expected, "Hello cannot send discarded traffic");

    tether.eth_in(&mut gw, 1, &dhcp(1, None, [0; 4]));
    let fresh = gw.clone().frames_out(1);
    assert_eq!(fresh.len(), 1, "the restarted device receives a fresh DHCP offer");
    tether.poll(&mut gw, 1, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!((sent.len(), tether.stats().queue_len), (5, 1), "fresh output waits for new credit after Hello");
    tether.credit(1, 0);
    tether.poll(&mut gw, 1, &mut |frame| sent.push(frame.to_vec()));
    assert!(tether.stats().link_up, "even a zero ETH_CREDIT announces the new link lifetime");
    assert_eq!((tether.stats().credits, sent.len()), (0, 5), "a zero grant cannot authorize a frame");
    tether.credit(1, 1);
    tether.poll(&mut gw, 1, &mut |frame| sent.push(frame.to_vec()));
    assert_eq!(&sent[5..], fresh.as_slice(), "only fresh traffic is sent after reconnecting");
    assert_eq!(tether.stats().queue_len, 0, "the new lifetime drains normally");

    tether.hello(&mut gw, 2);
    let received_before = tether.stats().credits_received;
    tether.credit(2, u16::MAX);
    assert_eq!(tether.stats().credit_anomalies, 0, "the largest representable buffer pool is not anomalous");
    tether.credit(2, u16::MAX);
    assert_eq!(tether.stats().credits, 2 * u32::from(u16::MAX), "excess credit is retained in u32, not silently clamped to u16");
    assert_eq!(tether.stats().credit_anomalies, 1, "a grant exceeding the largest pool has a named counter");
    // u32::MAX = 65,537 grants of u16::MAX. These are counter-only calls;
    // no gateway polling, frame allocation or virtual-time loop is involved.
    let grants = u32::MAX / u32::from(u16::MAX);
    for _ in 2..grants { tether.credit(2, u16::MAX); }
    assert_eq!(tether.stats().credits, u32::MAX, "credit accumulation reaches the full u32 range without wrapping");
    tether.credit(2, 1);
    assert_eq!(tether.stats().credits, u32::MAX, "u32 credit overflow saturates rather than wrapping or panicking");
    assert_eq!(tether.stats().credit_anomalies, u64::from(grants), "every excessive grant, including saturation, is counted");
    assert_eq!(tether.stats().credits_received - received_before, u64::from(u32::MAX) + 1, "received-credit accounting retains the count lost to saturation");
}

#[test]
fn raw_link_queues_frames_and_preserves_fc1_lifetimes() {
    use fab_elec_mcu_netgw::{Link, tether::FrameLength};
    use watch::{Bad, Frame};

    let mut link = Link::new();
    let mut sent = Vec::new();
    let mut reader = watch::Reader::new();
    let hello = Event::Hello {
        proto: watch::PROTO, ports_version: 1, image: 1, clock_hz: 240_000_000, prescale: 1,
        ring: 256, tick_hi: 0, mcusr: 0, wdtcsr: 0, level: watch::Level::Ports, generation: 0,
    };
    let device_packet = |link: &mut Link, seq: u8, tick: u32, event: Event| {
        let kind = match event {
            Event::Hello { .. } => watch::HELLO,
            Event::EthCredit { .. } => watch::ETH_CREDIT,
            Event::Probe { .. } => watch::PROBE,
            _ => panic!("raw control fixture uses Hello, ETH_CREDIT or Probe"),
        };
        let frame = Frame { kind, seq, tick, event };
        let mut wire = [0; watch::MAX_WIRE];
        let len = watch::encode(&frame, &mut wire);
        let decoded: Vec<_> = wire[..len].iter().filter_map(|&byte| link.push_raw(0, byte)).collect();
        assert_eq!(decoded, vec![Ok(frame)], "raw push returns each decoded control frame unchanged");
    };
    let frames = [
        ethernet(0x88b5, &[], false),
        ethernet(0x88b6, &(0..1500).map(|i| (i * 37) as u8).collect::<Vec<_>>(), false),
    ];
    let epoch = 0x1234_5678;
    device_packet(&mut link, 0, epoch, hello);
    for frame in &frames { link.queue_frame(frame).expect("both inclusive FC1 lengths can be queued"); }
    assert_eq!((link.stats().tether.queue_len, link.stats().tether.queue_high_water), (2, 2),
        "raw frames join the shared queue immediately and update its high-water mark");
    link.poll_raw(0, &mut |packet| sent.push(packet.to_vec()));
    assert!(sent.is_empty(), "queued raw frames wait while the link is down");
    device_packet(&mut link, 1, epoch + 1, Event::EthCredit { count: 0 });
    link.poll_raw(0, &mut |packet| sent.push(packet.to_vec()));
    assert!(link.stats().tether.link_up, "a zero credit grant brings the raw link up");
    assert!(sent.is_empty(), "link-up alone cannot send a queued frame without credit");
    for seq in [2, 3] {
        device_packet(&mut link, seq, epoch + u32::from(seq), Event::EthCredit { count: 1 });
        link.poll_raw(0, &mut |packet| sent.push(packet.to_vec()));
        assert_eq!(sent.len(), usize::from(seq - 1), "each granted credit releases exactly one queued frame");
        assert_eq!(link.stats().tether.credits, 0, "sending each raw frame spends its credit");
        link.poll_raw(0, &mut |packet| sent.push(packet.to_vec()));
        assert_eq!(sent.len(), usize::from(seq - 1), "exhausted credit cannot release or repeat a frame");
    }

    let before = link.stats();
    for len in [0, 13, 1515] {
        let error = link.queue_frame(&vec![0; len]).expect_err("invalid FC1 lengths are refused");
        assert_eq!(error, FrameLength { len }, "queue refusal exposes the supplied frame length");
        let error: &dyn std::error::Error = &error;
        assert!(error.to_string().contains(&len.to_string()), "the public error displays the refused length");
        assert_eq!(link.stats(), before, "a refused queue_frame changes neither the queue nor statistics");
    }
    link.poll_raw(0, &mut |packet| sent.push(packet.to_vec()));
    assert_eq!(link.stats().tether.queue_len, 0, "refused frames leave nothing queued");

    for frame in &frames { link.queue_frame(frame).expect("valid frames queue for the Hello discard check"); }
    device_packet(&mut link, 4, epoch + 4, Event::EthCredit { count: 7 });
    let next_epoch = epoch + 100;
    device_packet(&mut link, 5, next_epoch, hello);
    let reset = link.stats().tether;
    assert_eq!((reset.credits, reset.link_up, reset.queue_len), (0, false, 0),
        "raw Hello forgets credit, drops the link and empties the queue");
    assert_eq!((reset.hellos, reset.frames_discarded_on_hello), (2, 2),
        "raw Hello counts exactly the queued frames it discarded");
    assert_eq!((reset.frames_sent, reset.queue_high_water), (2, 2),
        "raw Hello preserves send history and queue high-water history");
    link.queue_frame(&frames[0]).expect("fresh output queues after Hello");
    link.poll_raw(0, &mut |packet| sent.push(packet.to_vec()));
    assert_eq!(sent.len(), 2, "fresh output cannot use the previous lifetime's credits");
    device_packet(&mut link, 6, next_epoch + 1, Event::EthCredit { count: 1 });
    link.poll_raw(0, &mut |packet| sent.push(packet.to_vec()));
    assert_eq!(sent.len(), 3, "new credit sends only fresh output after Hello");
    let expected = [(&frames[0], epoch), (&frames[1], epoch), (&frames[0], next_epoch)];
    for (seq, (packet, (body, tick))) in sent.iter().zip(expected).enumerate() {
        let mut count = 0;
        for &byte in packet {
            if let Some(result) = reader.push(byte) {
                let frame = result.expect("raw host output passes the watch codec");
                assert_eq!(frame, Frame { kind: watch::ETH, seq: seq as u8, tick, event: Event::Eth { len: body.len() as u16 } },
                    "raw host ETH echoes Hello's epoch and keeps consecutive seqs across Hello");
                assert_eq!(reader.eth_body(), body.as_slice(), "raw output is byte-exact in FIFO order");
                count += 1;
            }
        }
        assert_eq!(count, 1, "each raw send callback carries exactly one complete ETH packet");
    }
    assert_eq!((link.stats().tether.queue_len, link.stats().unencodable), (0, 0),
        "all accepted frames drain and fit the FC1 encoder");

    let mut received = Vec::new();
    for (index, body) in [&frames[1], &frames[0]].into_iter().enumerate() {
        let seq = 7 + index as u8;
        let mut wire = [0; watch::ETH_WIRE];
        let len = watch::encode_eth(seq, next_epoch + 2, body, &mut wire).expect("device boundary frame fits FC1");
        for &byte in &wire[..len] {
            if let Some(result) = link.push_raw(0, byte) {
                assert_eq!(result, Ok(Frame { kind: watch::ETH, seq, tick: next_epoch + 2, event: Event::Eth { len: body.len() as u16 } }),
                    "raw push returns device ETH metadata unchanged");
                received.push(link.eth_body().to_vec());
            }
        }
    }
    assert_eq!(received, vec![frames[1].clone(), frames[0].clone()], "raw device ETH bodies arrive byte-exact in device order");
    assert_eq!((link.stats().tether.frames_in, link.stats().tether.bad_lengths), (2, 0),
        "both valid device boundary lengths count as input without bad lengths");

    // Reader rejects malformed ETH lengths before a decoded frame can reach
    // Tether, just as it does for push with a Gateway.
    let before = link.stats();
    for len in [13, 1515] {
        let mut body = vec![watch::ETH, 9, 0, 0, 0, 0];
        body.resize(6 + len, 0);
        body.push(watch::crc8(&body));
        let mut wire = [0; watch::ETH_WIRE];
        let encoded = watch::cobs_encode(&body, &mut wire).expect("malformed boundary frame fits the wire buffer");
        wire[encoded] = 0;
        let errors: Vec<_> = wire[..=encoded].iter().filter_map(|&byte| link.push_raw(0, byte)).collect();
        assert_eq!(errors, vec![Err(Bad::Length)], "raw push returns the reader's invalid ETH length error");
        assert_eq!(link.stats(), before, "undecodable lengths never reach tether accounting");
    }
    assert_eq!(link.reader().bad.length, 2, "reader statistics count both malformed device ETH lengths");
    device_packet(&mut link, 9, next_epoch + 3, Event::Probe { id: 1, value: 42 });
    assert_eq!(link.stats(), before, "other watch events are returned without changing tether state");
    assert_eq!(link.reader().gaps, 0, "raw receive preserves device ordering across controls, ETH and rejected packets");
}

#[test]
fn radio_restart_waits_for_hello_and_shares_the_ethernet_sequence() {
    fn header(packet: &[u8]) -> [u8; 6] {
        let packet = packet.strip_suffix(&[0]).expect("host packet has a delimiter");
        let mut body = [0; watch::ETH_BODY];
        let len = watch::cobs_decode(packet, &mut body).expect("host packet COBS decodes");
        body[..len][..6].try_into().unwrap()
    }

    let mut r = Rig::new(None);
    r.link = Some(Device::new());
    let hello = Event::Hello {
        proto: watch::PROTO, ports_version: 1, image: 1, clock_hz: 240_000_000, prescale: 1,
        ring: 256, tick_hi: 0, mcusr: 0, wdtcsr: 0, level: watch::Level::Ports, generation: 0,
    };
    r.link.as_mut().unwrap().link.queue_radio_restart();
    r.device_packet(Event::EthCredit { count: 1 });
    r.link.as_mut().unwrap().link.poll_raw(r.now, &mut |_| panic!("credit alone does not establish an epoch"));

    r.device_packet(hello);
    r.now = 1;
    let link = &mut r.link.as_mut().unwrap().link;
    let mut sent = Vec::new();
    link.poll_raw(r.now, &mut |packet| sent.push(header(packet)));
    assert_eq!(sent, [[watch::RADIO_RESTART, 0, 0, 0, 0, 0]], "boot tick zero releases the request without credit");
    assert_eq!((link.stats().radio_restarts_sent, link.stats().radio_restarts_dropped_on_hello), (1, 0),
        "the first Hello preserves the pre-Hello request");

    link.queue_radio_restart();
    r.now = 0x1234_5678;
    r.device_packet(hello);
    let link = &mut r.link.as_mut().unwrap().link;
    link.poll(&mut r.gateway, r.now, &mut |_| panic!("the new Hello discarded the pending restart"));
    assert_eq!((link.stats().radio_restarts_sent, link.stats().radio_restarts_dropped_on_hello), (1, 1),
        "the pending request counts as one drop on the next Hello");

    r.now += 1;
    r.device_packet(Event::EthCredit { count: 1 });
    let link = &mut r.link.as_mut().unwrap().link;
    link.queue_frame(&ethernet(0x88b5, &[], false)).unwrap();
    link.queue_radio_restart();
    link.queue_radio_restart();
    sent.clear();
    link.poll(&mut r.gateway, r.now, &mut |packet| sent.push(header(packet)));
    assert_eq!(sent, [
        [watch::RADIO_RESTART, 1, 0x78, 0x56, 0x34, 0x12],
        [watch::ETH, 2, 0x78, 0x56, 0x34, 0x12],
    ], "one restart goes first, then Ethernet spends the credit with the next seq and the same Hello tick");
    link.poll_raw(r.now, &mut |_| panic!("both queued restart requests were served by one send"));
    assert_eq!((link.stats().radio_restarts_sent, link.stats().radio_restarts_dropped_on_hello), (2, 1),
        "the counters cover both Hello lifetimes and both poll modes");
}

#[test]
fn queued_frames_interleave_with_gateway_output() {
    let mut r = Rig::new(None);
    r.link = Some(Device::new());
    r.now = 17;
    let hello = Event::Hello {
        proto: watch::PROTO, ports_version: 1, image: 1, clock_hz: 240_000_000, prescale: 1,
        ring: 256, tick_hi: 0, mcusr: 0, wdtcsr: 0, level: watch::Level::Ports, generation: 0,
    };
    r.device_packet(hello);
    r.frame_in(&arp());
    let arp_reply = r.gateway.clone().frames_out(r.now);
    assert_eq!(arp_reply.len(), 1, "the ordinary gateway path produces one ARP reply");
    let first = ethernet(0x88b5, &[1, 2, 3], false);
    let second = ethernet(0x88b6, &[4, 5, 6], false);
    r.link.as_mut().unwrap().link.queue_frame(&first).expect("raw output queues in gateway mode");
    r.link.as_mut().unwrap().link.poll_raw(r.now, &mut |_| panic!("down link cannot send queued output"));
    assert_eq!((r.gateway.stats().frames_out, r.link.as_ref().unwrap().link.stats().tether.queue_len), (0, 1),
        "poll_raw leaves pending gateway output uncollected when modes are mixed");
    r.pump();
    assert_eq!(r.link.as_ref().unwrap().link.stats().tether.queue_len, 2,
        "gateway output joins behind a raw frame queued before collection");
    r.link.as_mut().unwrap().link.queue_frame(&second).expect("later raw output joins the same queue");
    r.frame_in(&ping());
    let ping_reply = r.gateway.clone().frames_out(r.now);
    assert_eq!(ping_reply.len(), 1, "the ordinary gateway path still produces one ICMP reply");
    r.pump();
    assert!(r.inbox.is_empty(), "all interleaved output waits for credit");
    assert_eq!(r.link.as_ref().unwrap().link.stats().tether.queue_len, 4,
        "raw and gateway frames share one queue before credit arrives");
    let expected = vec![first, arp_reply[0].clone(), second, ping_reply[0].clone()];
    r.device_packet(Event::EthCredit { count: 2 });
    r.pump();
    assert_eq!(r.inbox, expected[..2], "credit sends the first raw frame before the earlier-created but later-collected ARP reply");
    r.pump();
    assert_eq!(r.inbox, expected, "replenished credit preserves raw, gateway, raw, gateway FIFO order byte-for-byte");
    let device = r.link.as_ref().unwrap();
    assert_eq!((device.reader.frames, device.reader.gaps), (4, 0), "both output sources share one consecutive host sequence");
    assert_eq!((device.link.stats().tether.frames_sent, device.link.stats().tether.queue_len), (4, 0),
        "both output sources drain through the same tether accounting");
    assert_eq!((r.gateway.stats().frames_in, r.gateway.stats().frames_out, r.gateway.stats().device_resets), (2, 2, 1),
        "queue_frame leaves gateway input, replies and Hello reset behavior unchanged");

    let device = r.link.as_mut().unwrap();
    let seq = device.link.reader().frames as u8;
    let mut wire = [0; watch::ETH_WIRE];
    let len = watch::encode_eth(seq, 18, &arp(), &mut wire).expect("raw ARP request fits FC1");
    for &byte in &wire[..len] { let _ = device.link.push_raw(r.now, byte); }
    assert_eq!((device.link.stats().tether.frames_in, r.gateway.stats().frames_in), (3, 2),
        "only push feeds the gateway when raw and gateway receive modes are mixed");
    let mut wire = [0; watch::MAX_WIRE];
    let len = watch::encode(&watch::Frame { kind: watch::HELLO, seq: seq.wrapping_add(1), tick: 19, event: hello }, &mut wire);
    for &byte in &wire[..len] { let _ = device.link.push_raw(r.now, byte); }
    assert_eq!(r.gateway.stats().device_resets, 1, "raw Hello does not reset a previously used gateway");
    assert_eq!(r.gateway.learned_device_mac(), Some(DEVICE_MAC), "raw Hello leaves gateway device state intact");
    assert_eq!((device.link.stats().tether.hellos, device.link.stats().tether.credits, device.link.stats().tether.link_up), (2, 0, false),
        "raw Hello still starts a fresh tether lifetime when modes are mixed");
}

#[test]
fn link_carries_a_session_through_the_fc1_codec() {
    const BUFFERS: u16 = 4;
    const BYTES: usize = 64 * 1024;
    let mut r = Rig::new(None);
    r.link = Some(Device::new());
    r.link.as_mut().unwrap().corrupt_next_eth = true;
    r.frame_in(&arp());
    let corrupt_bad = r.link.as_ref().unwrap().link.reader().bad;
    assert_eq!(corrupt_bad, watch::BadCounts { crc: 1, ..watch::BadCounts::default() },
        "the deliberately corrupted ETH packet fails only its CRC");
    assert_eq!(r.link.as_ref().unwrap().link.stats().tether.frames_in, 0, "corrupt ETH never reaches the tether");
    assert_eq!(r.gateway.stats().frames_in, 0, "corrupt ETH never reaches the gateway");

    let hello = Event::Hello {
        proto: watch::PROTO, ports_version: 1, image: 1, clock_hz: 240_000_000, prescale: 1,
        ring: 256, tick_hi: 0, mcusr: 0, wdtcsr: 0, level: watch::Level::Ports, generation: 0,
    };
    r.device_packet(hello);
    r.device_packet(Event::EthCredit { count: BUFFERS });
    // Rig returns one credit packet after consuming each frame; every poll
    // can therefore deliver at most these four physical receive buffers.
    let offer = r.exchange(&dhcp(1, None, [0; 4]), |f| {
        udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[2]))
    });
    assert_eq!(&udp_payload(&offer, 67).unwrap()[16..20], &DEVICE_IP, "DHCP offer crosses the credited tether");
    let ack = r.exchange(&dhcp(3, Some(DEVICE_IP), [0; 4]), |f| {
        udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[5]))
    });
    assert_eq!(&udp_payload(&ack, 67).unwrap()[16..20], &DEVICE_IP, "DHCP request completes through the tether");

    let (index, conn) = r.connect();
    let data: Vec<u8> = (0..BYTES).map(|i| (i * 37 ^ (i >> 8)) as u8).collect();
    r.clients[index].queue(ws(2, true, true, &data));
    r.until("64 KiB crosses both ways with four receive buffers", |r| {
        !r.clients[index].messages.is_empty() && r.clients[index].idle()
            && r.gateway.host_pending(conn) == 0 && r.link.as_ref().unwrap().link.stats().tether.queue_len == 0
    });
    let expected = vec![Message::Binary(data)];
    assert_eq!(r.clients[index].messages, expected, "the entire 64 KiB echo reaches the device once, byte-exact");
    let delivered: Vec<_> = r.events.iter().filter_map(|event| match event {
        HostEvent::Message { conn: id, msg } if *id == conn => Some(msg.clone()), _ => None,
    }).collect();
    assert_eq!(delivered, expected, "the host receives the entire 64 KiB device message once, byte-exact");
    r.clients[index].close_device();
    r.until("scarce-credit session closes cleanly", |r| r.clients[index].peer_fin && r.clients[index].fin_acked);
    r.pump();
    assert_eq!(r.clients[index].close.as_ref().map(|v| v.0), Some(1000), "WebSocket close also crosses the tether");
    assert!(r.events.iter().any(|event| matches!(event, HostEvent::Closed { conn: id, code: 1000, .. } if *id == conn)),
        "the host observes a normal session close");

    let stats = r.link.as_ref().unwrap().link.stats().tether;
    let gateway = r.gateway.stats();
    assert_eq!((gateway.ws_bytes_in, gateway.ws_bytes_out), (BYTES as u64, BYTES as u64),
        "both directions account for all 64 KiB");
    assert_eq!((gateway.ws_messages_in, gateway.ws_messages_out), (1, 1), "scarce credits cannot lose or duplicate messages");
    assert_eq!((stats.bad_lengths, stats.credit_anomalies, stats.frames_discarded_on_hello), (0, 0, 0),
        "the session has no tether refusals, credit anomalies or discards");
    assert_eq!((stats.frames_in, stats.frames_sent), (gateway.frames_in, gateway.frames_out),
        "every gateway frame crosses the tether without loss");
    assert_eq!((gateway.faults_in.dropped, gateway.faults_out.dropped, gateway.tcp_out_of_window), (0, 0, 0),
        "the session has no gateway drops");
    assert_eq!((gateway.tcp_retransmits, r.clients[index].retransmits), (0, 0),
        "retransmissions cannot conceal a dropped tether frame");
    assert_eq!((stats.queue_len, stats.credits, stats.link_up), (0, u32::from(BUFFERS), true),
        "all frames drain and all four receive buffers return");
    assert_eq!(stats.credits_received, stats.frames_sent + u64::from(BUFFERS),
        "only consumed frames replenish the initial credit grant");
    // One connection has a 65,535-byte window and the client advertises MSS
    // 1200. Allow two frames per MSS of window (a full segment plus a short
    // send-boundary tail) and eight control frames for DHCP, ACKs and setup/
    // close. This sequential upload/echo has at most an 8 KiB device flight;
    // its ACKs drain four at a time. No retransmissions add queued copies.
    // The resulting bound is 2 * ceil(65535 / 1200) + 8 = 118 frames.
    const QUEUE_BOUND: usize = 2 * ((65_535 + 1199) / 1200) + 8;
    assert!(stats.queue_high_water > usize::from(BUFFERS),
        "the session exercises a backlog larger than the receive-buffer pool");
    assert!(stats.queue_high_water < QUEUE_BOUND,
        "queue high-water {} stays below the window-derived bound of {QUEUE_BOUND} frames", stats.queue_high_water);

    let check_codec = |r: &Rig, hellos: u64, grants: u64| {
        let device = r.link.as_ref().unwrap();
        let link = device.link.stats();
        let reader = device.link.reader();
        assert_eq!(link.unencodable, 0, "every gateway frame fits the FC1 codec");
        assert_eq!(device.reader.gaps, 0, "host sequence numbers are consecutive, including across wraps and Hello");
        assert_eq!(device.reader.frames, link.tether.frames_sent, "every released host frame decodes at the device");
        assert_eq!(device.reader.bad, watch::BadCounts::default(), "the device decodes every host packet without errors");
        assert_eq!(device.reader.junk, 0, "the host sends no overlong packets");
        assert_eq!(reader.bad, corrupt_bad, "the deliberate CRC failure is the only bad device packet");
        assert_eq!((reader.junk, reader.gaps), (0, 0), "valid device traffic has no junk or sequence gaps");
        assert_eq!(link.tether.hellos, hellos, "each decoded Hello starts one tether lifetime");
        assert_eq!(reader.frames, link.tether.frames_in + link.tether.frames_sent + grants + hellos,
            "decoded frames = device ETH frames + credit packets (consumed frames + grants) + {hellos} Hello packet(s)");
    };
    // Check before the next Hello can reset the device stream's gap count.
    check_codec(&r, 1, 1);

    let (_, second_conn) = r.connect();
    // Five device requests guarantee a queued reply after spending four
    // credits, independently of TCP's delayed ACKs on the open connection.
    for _ in 0..=BUFFERS { r.frame_in(&ping()); }
    r.pump();
    let queued = r.link.as_ref().unwrap().link.stats().tether.queue_len;
    assert!(queued > 0, "the second Hello arrives with gateway replies waiting in the tether");
    check_codec(&r, 1, 1);
    assert_ne!(r.now as u32, 0, "the second Hello's epoch differs from the first's, so the echo is observable");
    r.device_packet(hello);
    r.events.extend(r.gateway.host_events(r.now));
    assert!(r.events.iter().any(|event| matches!(event,
        HostEvent::Closed { conn: id, code: 1006, .. } if *id == second_conn)),
        "the second Hello closes the open WebSocket for the host with code 1006");
    let reset = r.link.as_ref().unwrap().link.stats().tether;
    assert_eq!((reset.credits, reset.link_up, reset.queue_len), (0, false, 0),
        "the second Hello forgets credits, drops the link and clears its queue");
    assert_eq!(reset.frames_discarded_on_hello, queued as u64, "Hello discards exactly the replies queued at reset");

    // A reboot forgets the device's old TCP clients and already received frames.
    r.clients.clear();
    r.inbox.clear();
    r.device_packet(Event::EthCredit { count: 1 });
    let fresh = r.exchange(&dhcp(1, None, [0; 4]), |f| {
        udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[2]))
    });
    assert_eq!(&udp_payload(&fresh, 67).unwrap()[16..20], &DEVICE_IP, "the new lifetime receives a fresh DHCP offer");
    let fresh_stats = r.link.as_ref().unwrap().link.stats().tether;
    assert_eq!((fresh_stats.queue_len, fresh_stats.credits, fresh_stats.link_up), (0, 1, true),
        "the fresh DHCP exchange drains and returns its single receive buffer");
    check_codec(&r, 2, 2);
}

#[test]
fn websocket_validation_and_dhcp_modes() {
    let mut r = Rig::new(None);
    let empty = r.exchange(&dns("bridge.invalid", 28), |f| udp_payload(f, 53).is_some());
    let empty = udp_payload(&empty, 53).unwrap();
    assert_eq!((read16(empty, 2) & 15, read16(empty, 6)), (0, 0), "configured non-A DNS name has NOERROR without answers");
    for address in [[0; 4], DEVICE_IP] {
        let request = dhcp(3, if address == [0; 4] { Some(DEVICE_IP) } else { None }, address);
        r.exchange(&request, |f| udp_payload(f, 67).is_some_and(|d| dhcp_option(d, 53) == Some(&[5])));
    }
    r.gateway.frame_in(r.now, &dhcp(7, None, DEVICE_IP));
    assert_eq!(r.gateway.stats().dhcp_releases, 1, "DHCP RELEASE frees the lease");
    assert_eq!(r.gateway.next_deadline(), None, "released lease leaves no expiration timer");

    for (frames, expected) in [
        (vec![ws(1, true, true, &[0xff])], 1007),
        (vec![ws(2, false, true, b"first"), ws(2, true, true, b"second")], 1002),
        (vec![ws(0, true, true, b"orphan")], 1002),
        (vec![ws(9, false, true, b"fragmented ping")], 1002),
    ] {
        let (index, _) = r.connect();
        for frame in frames { r.clients[index].queue(frame); }
        r.until("invalid WebSocket receives a close", |r| r.clients[index].close.is_some());
        assert_eq!(r.clients[index].close.as_ref().map(|v| v.0), Some(expected), "WebSocket validation chooses the required close code");
        r.until("invalid WebSocket closes transport", |r| r.clients[index].peer_fin && r.clients[index].fin_acked);
    }
    assert_eq!(r.gateway.stats().ws_utf8, 1, "invalid UTF-8 has its own counter");
    assert_eq!(r.gateway.stats().ws_protocol, 3, "invalid fragmentation is counted");

    let index = r.clients.len();
    r.clients.push(Client::new(40000 + index as u16));
    r.until("bad HTTP fixture opens TCP", |r| r.clients[index].connected);
    r.clients[index].queue(b"GET / HTTP/1.1\r\nHost: 192.0.2.80\r\n\r\n".to_vec());
    r.until("bad upgrade receives HTTP response and FIN", |r| !r.clients[index].response.is_empty() && r.clients[index].peer_fin);
    assert!(r.clients[index].response.starts_with("HTTP/1.1 400 "), "bad upgrade receives 400 Bad Request");
    assert_eq!(r.gateway.stats().http_bad_request, 1, "HTTP refusal has its own counter");
}

#[test]
fn published_hash_encoding_and_upgrade_vectors() {
    for (input, expected) in [
        ("", "da39a3ee5e6b4b0d3255bfef95601890afd80709"),
        ("abc", "a9993e364706816aba3e25717850c26c9cd0d89d"),
        ("abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq", "84983e441c3bd26ebaae4aa1f95129e5e54670f1"),
    ] {
        let digest: String = sha1::digest(input.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, expected, "SHA-1 published vector for {input:?}");
    }
    for (input, expected) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"), ("foob", "Zm9vYg=="), ("fooba", "Zm9vYmE="), ("foobar", "Zm9vYmFy")] {
        assert_eq!(base64::encode(input.as_bytes()), expected, "RFC 4648 encoder vector for {input:?}");
        assert_eq!(base64::decode(expected), Some(input.as_bytes().to_vec()), "RFC 4648 decoder vector for {input:?}");
    }
    for invalid in ["Zg=", "Zh==", "Zm9=", "Zg==AAAA", "!!!!", "Zg==\n"] {
        assert_eq!(base64::decode(invalid), None, "base64 rejects malformed or noncanonical input {invalid:?}");
    }
    assert_eq!(websocket_accept("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=", "RFC 6455 handshake vector");
}

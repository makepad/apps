//! F5/F6: a real S3 image exchanges Ethernet with netgw's real raw Link.
//! Only the USB host byte pipe carries traffic; SRAM reads observe the app's
//! eth_recv result and counters. No image patch, model-only port or Gateway.
use std::sync::Arc;

use fab_elec_mcu::compile;
use fab_elec_mcu_emit_s3::{
    build_image, chip, runtime, Board, BoardTime, Image, SlotOptions, SLOT_BASE, VOICEBOX,
};
use fab_elec_mcu_emu::{Machine, PcSet, Ran, Until};
use fab_elec_mcu_netgw::Link;
use fab_elec_mcu_rt::{module, watch};
use fab_elec_mcu_s3::{CpuClock, PinsProfile, S3Machine, S3Profile, TimeProfile};

const RX_SLOTS: u32 = 2;
const WRAP_BYTES: usize = 8 * runtime::TX_RING_BYTES as usize;
const SIZES: [usize; 4] = [14, 1514, 254, 1024];
const MIC_RATE: u64 = 32_000;
const SPEAKER_RATE: u64 = 38_000;
// 20 ms of 16 kHz, 16-bit mono is 640 bytes, plus an Ethernet header.
// This offers 32000 audio B/s (32700 Ethernet B/s), without small-frame
// framing overhead dominating the microphone's ordinary voice cadence.
const MIC_PERIOD_MS: u32 = 20;
const MIC_FRAME_BYTES: usize = 14 + (MIC_RATE * MIC_PERIOD_MS as u64 / 1000) as usize;
// Slots bound frames in flight. Use the mic payload rate as a conservative
// drain-rate floor: 8192 / 32000 = 256 ms behind a full TX ring. Add
// ceil(1528 / 64) = 24 ms for the down frame and 1 ms for its credit.
// ceil(38000 * (0.256 + 0.024 + 0.001) / 1514) = 8 slots, or 12112
// payload bytes. Both full-duplex counts and the app mix use the same
// adverse 1 ms service assumption; the first board run must replace it.
const CREDIT_ROUND_TRIP_MS: u64 = (runtime::TX_RING_BYTES as u64 * 1000).div_ceil(MIC_RATE)
    + (watch::ETH_WIRE as u64).div_ceil(64) + 1;
const APP_RX_SLOTS: u32 = (SPEAKER_RATE * CREDIT_ROUND_TRIP_MS)
    .div_ceil(1000 * watch::ETH_MAX as u64) as u32;

fn profile() -> S3Profile {
    S3Profile {
        flash_bytes: VOICEBOX.flash_bytes as usize,
        handoff_cpu: CpuClock::Xtal { div: 2 },
        handoff_mapped_pages: 64,
        entry: 0x4200_0408,
        seed: 53,
       
        mac: [2, 0x13, 0x80, 0xff, 0, 0x42], pvt: fab_elec_mcu_s3::NO_PVT,
        time: TimeProfile {
            systimer_first_long: true,
            rc_slow_hz: (136_000, 1),
            wdt_delay_sel: 0,
            swd_timeout_ticks: 432_000_000,
            swd_request_lead_ticks: 48_000_000,
            wdt_sync_cycles: 2,
            mwdt_rephase_on_feed: false,
            mwdt_rephase_on_config: true,
            wdt_restart_on_enable: true,
        },
        pins: PinsProfile {
            power_on_gpio0_high: true,
            usb_service_ticks: 480_000,
            usb_frame_phase_ticks: 0,
            input_sync_delay_ticks: 0,
            handoff_digital_pads_unheld: true,
            handoff_out: [0; 2],
            handoff_enable: [0; 2],
            handoff_pin: [0; 45],
            handoff_io_mux: [1 << 12; 45],
            handoff_out_sel: [256; 45],
            handoff_usb_tx: Vec::new(),
            handoff_usb_tx_flushed: false,
            handoff_usb_rx: Vec::new(),
            handoff_usb_int_raw: 8,
            handoff_usb_int_ena: 0,
        },
    }
}

fn bytes(m: &S3Machine, address: u32, len: usize) -> Vec<u8> {
    let mut result = vec![0; len];
    assert!(m.read_data(address, &mut result), "fixture SRAM is readable");
    result
}

fn word(m: &S3Machine, address: u32) -> u32 {
    u32::from_le_bytes(bytes(m, address, 4).try_into().unwrap())
}

fn pair(m: &S3Machine, address: u32) -> u64 {
    u64::from_le_bytes(bytes(m, address, 8).try_into().unwrap())
}

fn frame(len: usize, serial: usize, seed: u8) -> Vec<u8> {
    // All byte values, including zero and COBS block boundaries, recur.
    // Even a minimum Ethernet header distinguishes its position in the stream.
    let mut data: Vec<_> = (0..len)
        .map(|i| (i as u8).wrapping_add((serial as u8).wrapping_mul(37)).wrapping_add(seed))
        .collect();
    data[0] = 2;
    data[6] = 2;
    data[12..14].copy_from_slice(&[0x88, 0xb5]);
    data
}

fn transfer(total: usize, seed: u8) -> Vec<Vec<u8>> {
    let mut frames = Vec::new();
    let mut remaining = total;
    while remaining != 0 {
        let len = SIZES[frames.len() % SIZES.len()].min(remaining);
        assert!(len >= watch::ETH_MIN, "fixture ends with a whole Ethernet frame");
        frames.push(frame(len, frames.len(), seed));
        remaining -= len;
    }
    frames
}

struct Rig {
    image: Image,
    machine: S3Machine,
    link: Link,
    marks: PcSet,
    recv_return: u32,
    rx_buffer: u32,
    rx_slots: u32,
    state: Vec<(String, u32)>,
    hellos: Vec<watch::Frame>,
    credits: Vec<(usize, u16)>,
    reports: [Vec<u64>; 5],
    tx: Vec<Vec<u8>>,
    rx: Vec<Vec<u8>>,
    last_tx: u64,
    last_rx: u64,
    device_seq: Option<u8>,
    host_seq: u8,
}

impl Rig {
    fn new(total: usize, rx_slots: u32, frame_bytes: Option<usize>, period_ms: u32) -> Self {
        // Only ordinary source constants vary; the image has no model-only
        // port or flag. Keep F5's mixed lengths and fill work unchanged.
        let choose_length = frame_bytes.map_or_else(|| String::from(
            "pending = 14\n\
             if sent % 4 == 1 { pending = 1514 }\n\
             if sent % 4 == 2 { pending = 254 }\n\
             if sent % 4 == 3 { pending = 1024 }"
        ), |len| format!("pending = {len}"));
        let source = include_str!("support/link.splash")
            .replace("TRANSFER_BYTES", &total.to_string())
            .replace("RX_SLOTS", &rx_slots.to_string())
            .replace("SEND_PERIOD_MS", &period_ms.to_string())
            .replace("CHOOSE_LENGTH", &choose_length)
            .replace("FILL_BYTES", &frame_bytes.unwrap_or(watch::ETH_MAX).to_string());
        let language = chip::profile();
        let compiled = compile(&source, &language, &[], &[]).expect("exchange program compiles");
        let module = module::load(&compiled.module, &language).expect("exchange module verifies");
        let profile = profile();
        let board = Board {
            layout: VOICEBOX,
            panel: None,
            i2c: None,
            motion: None, touch: None,
            radio: None,
            time: BoardTime {
                rc_slow_hz: profile.time.rc_slow_hz,
                wdt_delay_sel: profile.time.wdt_delay_sel,
            },
        };
        let image = build_image(&module, &board, &SlotOptions::default()).expect("exchange image links");
        let recv_return = image.slot.map.runtime.iter().find(|f| f.name == "eth_recv").unwrap().end - 2;
        let mut marks = PcSet::new(SLOT_BASE, image.slot.bytes.len() as u32);
        marks.insert(recv_return);
        let rx_buffer = image.slot.layout.buffers.iter().find(|b| b.name == "rx").unwrap().address;
        let state = compiled.lowered.state_vars.iter()
            .map(|v| (v.name.clone(), image.slot.layout.state + v.offset * 4)).collect();
        let machine = S3Machine::new(Arc::new(image.flash.clone()), &profile);
        Self {
            image, machine, link: Link::new(), marks, recv_return, rx_buffer, rx_slots, state,
            hellos: Vec::new(), credits: Vec::new(), reports: std::array::from_fn(|_| Vec::new()),
            tx: Vec::new(), rx: Vec::new(), last_tx: 0, last_rx: 0, device_seq: None, host_seq: 0,
        }
    }

    fn state(&self, name: &str) -> u32 {
        word(&self.machine, self.state.iter().find(|(n, _)| n == name).unwrap().1)
    }

    fn counters(&self) -> [u64; 5] {
        let e = self.image.slot.layout.link.as_ref().unwrap();
        [e.in_edges, e.eth_stale, e.tx_dropped, e.rx_bad, e.rx_refused]
            .map(|address| pair(&self.machine, address))
    }

    fn take(&mut self) -> usize {
        let mut out = Vec::new();
        self.machine.take_out(&mut out);
        let len = out.len();
        let now_ms = self.machine.now() / (self.machine.tick_hz() / 1000);
        for byte in out {
            assert_eq!(byte.link, 0, "all watch traffic uses the USB byte pipe");
            let Some(decoded) = self.link.push_raw(now_ms, byte.byte) else { continue; };
            let decoded = decoded.expect("every device frame passes Link's real COBS/CRC reader");
            if let Some(seq) = self.device_seq {
                assert_eq!(decoded.seq, seq.wrapping_add(1), "device frames have no sequence gaps");
            }
            self.device_seq = Some(decoded.seq);
            match decoded.event {
                watch::Event::Hello { .. } => {
                    self.hellos.push(decoded);
                    let tether = self.link.stats().tether;
                    assert!(!tether.link_up, "Hello starts down until the full grant");
                    assert_eq!((tether.credits, tether.queue_len), (0, 0), "Hello resets credits and queue");
                }
                watch::Event::Eth { len } => {
                    let body = self.link.eth_body();
                    assert_eq!(body.len(), usize::from(len));
                    self.tx.push(body.to_vec()); // Borrow ends before the next push_raw.
                    self.last_tx = byte.tick;
                }
                watch::Event::EthCredit { count } => {
                    let lifetime = self.hellos.len();
                    assert_ne!(lifetime, 0, "credit follows Hello");
                    if self.credits.last().is_none_or(|(epoch, _)| *epoch != lifetime) {
                        assert_eq!(u32::from(count), self.rx_slots, "each Hello has one full grant");
                    }
                    self.credits.push((lifetime, count));
                }
                watch::Event::Link { counter, value } => {
                    let reports = &mut self.reports[usize::from(counter)];
                    assert!(value > reports.last().copied().unwrap_or(0), "LINK snapshots increase");
                    reports.push(value);
                }
                watch::Event::Time { .. } => (),
                other => panic!("unexpected watch event in an Ethernet-only exchange: {other:?}"),
            }
        }
        self.machine.mark();
        assert!(self.machine.last_reset().is_none(), "exchange and backpressure never reset the device");
        len
    }

    fn packets(&mut self) -> Vec<Vec<u8>> {
        let mut packets = Vec::new();
        let now_ms = self.machine.now() / (self.machine.tick_hz() / 1000);
        self.link.poll_raw(now_ms, &mut |packet| packets.push(packet.to_vec()));
        for packet in &packets {
            let mut reader = watch::Reader::new();
            let mut frames = 0;
            for &byte in packet {
                if let Some(decoded) = reader.push(byte) {
                    let decoded = decoded.expect("Link's host packet passes COBS/CRC");
                    assert!(matches!(decoded.event, watch::Event::Eth { .. }));
                    assert_eq!(decoded.tick, self.hellos.last().unwrap().tick, "host echoes the live epoch");
                    assert_eq!(decoded.seq, self.host_seq, "host sequence continues across Hello");
                    self.host_seq = self.host_seq.wrapping_add(1);
                    frames += 1;
                }
            }
            assert_eq!(frames, 1, "poll supplies whole encoded frames");
        }
        packets
    }

    fn put(&mut self, packets: &[Vec<u8>]) {
        for packet in packets {
            for &byte in packet { self.machine.link_in(0, byte); }
        }
    }

    fn queue(&mut self, frames: &[Vec<u8>]) {
        for frame in frames { self.link.queue_frame(frame).expect("peer supplies Ethernet lengths"); }
    }

    fn step(&mut self, listening: bool, poll: bool) -> usize {
        self.machine.set_usb_host_listening(listening);
        let ticks = self.machine.tick_hz() / 1000;
        let end = (self.machine.now() / ticks + 1) * ticks;
        let mut output = 0;
        loop {
            let ran = self.machine.run(&Until {
                tick: end, marks: &self.marks, stops: &[self.recv_return],
                stops_after: self.machine.now(), on_drive: false,
            });
            assert!(matches!(ran, Ran::Pc | Ran::Tick | Ran::Barrier), "{ran:?}: {:?}", self.machine.status());
            output += self.take();
            if ran == Ran::Pc {
                // Observe the completed real port copy before its return, so
                // even two receives in one handler are checked individually.
                let len = self.machine.cpu().ar[2] as usize;
                if len != 0 {
                    assert!((watch::ETH_MIN..=watch::ETH_MAX).contains(&len));
                    self.rx.push(bytes(&self.machine, self.rx_buffer, len));
                    self.last_rx = self.machine.now();
                }
            }
            if poll {
                let packets = self.packets();
                self.put(&packets);
            }
            if ran == Ran::Tick { break; }
        }
        output
    }

    fn boot(&mut self) {
        while !self.link.stats().tether.link_up { self.step(true, true); }
        assert_eq!(self.hellos.len(), 1);
        assert_eq!(self.link.stats().tether.credits, self.rx_slots);
        assert_eq!(self.image.slot.layout.eth.as_ref().unwrap().count, self.rx_slots);
    }

    fn finish(&mut self, tx: &[Vec<u8>], rx: &[Vec<u8>]) {
        loop {
            let output = self.step(true, true);
            assert!(self.tx.len() <= tx.len() && self.rx.len() <= rx.len(), "no duplicate or unsolicited frames");
            let l = &self.image.slot.layout;
            let tether = self.link.stats().tether;
            // Drain the hardware packet as well as the software ring and
            // wait for the final counter snapshot and returned credits.
            if self.tx.len() == tx.len() && self.rx.len() == rx.len() && output == 0
                && word(&self.machine, l.tx_head) == word(&self.machine, l.tx_tail)
                && self.reports.each_ref().map(|v| v.last().copied().unwrap_or(0)) == self.counters()
                && tether.queue_len == 0 && tether.credits == self.rx_slots {
                break;
            }
        }
        assert_eq!(self.tx, tx, "device-to-host frames are byte-exact and ordered");
        assert_eq!(self.rx, rx, "host-to-device frames are byte-exact and ordered");
        assert_eq!(self.state("sent") as usize, tx.len());
        assert_eq!(self.state("received") as usize, rx.len());
        assert_eq!(self.state("sent_bytes") as usize, tx.iter().map(Vec::len).sum::<usize>());
        assert_eq!(self.state("received_bytes") as usize, rx.iter().map(Vec::len).sum::<usize>());
        let stats = self.link.stats();
        assert_eq!(stats.tether.frames_in as usize, tx.len());
        assert_eq!((stats.tether.bad_lengths, stats.tether.credit_anomalies, stats.unencodable), (0, 0, 0));
    }

    fn check_counters(&self, edges: u64, stale: u64) {
        let expected = [edges, stale, u64::from(self.state("retries")), 0, 0];
        assert_eq!(self.counters(), expected, "refused sends are retries, not lost Ethernet frames");
        let e = self.image.slot.layout.link.as_ref().unwrap();
        for (counter, value) in expected.into_iter().enumerate() {
            assert_eq!(self.reports[counter].last().copied().unwrap_or(0), value, "LINK counter {counter}");
            assert_eq!(pair(&self.machine, e.reported + counter as u32 * 8), value, "reported counter {counter}");
        }
        assert_eq!(word(&self.machine, self.image.slot.layout.lost_count), 0, "no watch events lost");
    }

    fn check_exchange(&self, received: usize) {
        self.check_counters(0, 0);
        let tether = self.link.stats().tether;
        assert_eq!(tether.hellos, 1, "continuous service keeps one lifetime");
        assert_eq!(tether.frames_sent as usize, received);
        assert_eq!(tether.frames_discarded_on_hello, 0);
        assert_eq!(tether.credits_received, self.rx_slots as u64 + received as u64);
    }
}

fn report_rates(case: &str, slots: u32, up: f64, down: f64) {
    eprintln!(
        "F6 {case}, {slots} RX slots: device->host {up:.0} B/s, host->device {down:.0} B/s; 330 KB picture {:.3} s; 32 KB/s mic {:.1}%; 38 KB/s speaker {:.1}% (decimal KB, Ethernet bytes, simulated time)",
        330_000.0 / down, 100.0 * MIC_RATE as f64 / up, 100.0 * SPEAKER_RATE as f64 / down,
    );
}

#[test]
fn link_exchanges_eight_tx_rings_with_backpressure_and_reports_rates() {
    let tx = transfer(WRAP_BYTES, 83);
    let rx = transfer(WRAP_BYTES, 197);
    let mut down_rates = [0.0; 2];
    for (index, slots) in [RX_SLOTS, APP_RX_SLOTS].into_iter().enumerate() {
        let mut r = Rig::new(WRAP_BYTES, slots, None, 4);
        r.boot();
        let started = r.machine.now();
        r.queue(&rx);

        // One USB service frame in four is a slow host, not a delayed take_out:
        // EP1 really stays busy. Each open is far inside the 250 ms edge window.
        // Stop this phase only once the app has observed a real send refusal.
        let mut service = 0;
        while r.state("retries") == 0 {
            r.step(service % 4 == 3, true);
            service += 1;
        }
        assert!(r.state("sent_bytes") < WRAP_BYTES as u32, "backpressure occurs during the transfer");
        assert_ne!(word(&r.machine, r.image.slot.layout.tx_head), word(&r.machine, r.image.slot.layout.tx_tail));
        r.finish(&tx, &rx);
        r.check_exchange(rx.len());

        // Payload rates include startup/backpressure and use only model ticks.
        // The host ceiling is 64 B/ms per direction before framing/credits.
        let hz = r.machine.tick_hz() as f64;
        let up = WRAP_BYTES as f64 * hz / (r.last_tx - started) as f64;
        let down = WRAP_BYTES as f64 * hz / (r.last_rx - started) as f64;
        report_rates("full duplex (slow start included)", slots, up, down);
        down_rates[index] = down;
    }
    assert!(down_rates[1] > down_rates[0],
        "{APP_RX_SLOTS} slots must improve down on simulated time: {} versus {} B/s with {RX_SLOTS} slots",
        down_rates[1], down_rates[0],
    );
}

#[test]
fn link_reopens_with_one_edge_and_rejects_old_lifetime_packets() {
    // Two rings suffice to leave queued TX bytes when the port closes.
    let tx = transfer(2 * runtime::TX_RING_BYTES as usize, 83);
    let prefix = vec![frame(14, 0, 197)];
    let mut r = Rig::new(tx.iter().map(Vec::len).sum(), RX_SLOTS, None, 4);
    r.boot();
    r.queue(&prefix);
    while r.tx.is_empty() || r.rx.len() != prefix.len() || r.link.stats().tether.credits != RX_SLOTS {
        r.step(true, true);
    }
    let old_hello = r.hellos[0];
    let e = r.image.slot.layout.eth.as_ref().unwrap().clone();
    let link = r.image.slot.layout.link.as_ref().unwrap().clone();
    assert!(r.state("sent_bytes") < 2 * runtime::TX_RING_BYTES, "close mid-transfer");

    // Real Link packets already written by a host can outlive that host's
    // knowledge of the epoch. Keep two in an OS-like byte buffer; the third
    // remains in Tether because both old credits have been spent.
    r.queue(&[frame(14, 1, 197), frame(254, 2, 197), frame(1514, 3, 197)]);
    let old_packets = r.packets();
    assert_eq!(old_packets.len(), RX_SLOTS as usize);
    assert_eq!(r.link.stats().tether.queue_len, 1);
    let closed = r.machine.now();
    // A stall window can have begun before close. Stay closed past 250 ms
    // from the close itself as well as waiting for the runtime's armed edge.
    let reopen_at = closed + r.machine.tick_hz() / 4 + r.machine.tick_hz() / 1000;
    while r.machine.now() < reopen_at || word(&r.machine, link.stall_state) != 2 {
        r.step(false, false);
    }
    assert!(r.machine.now() - closed > r.machine.tick_hz() / 4, "closed past 250 ms");
    assert_eq!(r.hellos.len(), 1, "closing alone causes no Hello");
    assert_eq!(pair(&r.machine, link.in_edges), 0);
    assert!(r.state("retries") > 0, "the closed port fills TX and refuses sends");

    while r.hellos.len() < 2 || r.credits.last().unwrap().0 != 2 { r.step(true, false); }
    assert_eq!(r.hellos[1].event, old_hello.event, "re-Hello preserves the boot identity");
    assert_ne!(r.hellos[1].tick, old_hello.tick);
    assert_eq!(word(&r.machine, link.epoch), r.hellos[1].tick);
    assert_eq!(word(&r.machine, e.received), 0, "device RX accounting starts a new lifetime");
    assert_eq!(r.link.stats().tether.frames_discarded_on_hello, 1);
    assert_eq!(r.link.stats().tether.credits, RX_SLOTS);

    let credits = r.link.stats().tether.credits_received;
    r.put(&old_packets);
    while pair(&r.machine, link.eth_stale) < old_packets.len() as u64 { r.step(true, false); }
    assert_eq!(r.rx, prefix, "old packets never reach eth_recv");
    assert_eq!(word(&r.machine, e.received), 0);
    assert_eq!(r.link.stats().tether.credits_received, credits, "stale packets earn no credit");

    // Per fc1-seam: old host queue/RX slots are not promised delivery in the
    // new lifetime. They must be discarded/counted, not replayed as new data.
    // The device TX ring *does* retain bytes. This exchange app also retains
    // its pending send and sequence state, unlike a network stack restarting
    // DHCP. Keep the same Link decoder: every committed TX byte, and all new
    // host frames after re-Hello, must still arrive exactly once and in order.
    let suffix = transfer(runtime::TX_RING_BYTES as usize, 219);
    r.queue(&suffix);
    let expected_rx: Vec<_> = prefix.into_iter().chain(suffix).collect();
    r.finish(&tx, &expected_rx);
    r.check_counters(1, old_packets.len() as u64);
    assert!(r.state("retries") > 0, "closed-port backpressure was retried");
    let tether = r.link.stats().tether;
    assert_eq!(tether.hellos, 2, "one reopen produces exactly one new lifetime");
    assert_eq!(tether.frames_sent as usize, expected_rx.len() + old_packets.len());
    assert_eq!(tether.frames_discarded_on_hello, 1);
    assert_eq!(tether.credits_received, 2 * RX_SLOTS as u64 + expected_rx.len() as u64);
    assert_eq!(r.reports[0], [1], "one IN edge on the wire as well as in SRAM");
}

#[test]
fn link_carries_microphone_while_receiving_saturated_pictures() {
    assert_eq!(APP_RX_SLOTS, 8, "slot budget must track the model's ring, service and app rates");
    // Eight picture frames warm the pipeline; the next 64 measure about
    // 1.5 s at the host ceiling, over five conservative credit round trips.
    // 160 microphone frames last 3.2 s, keeping up active even at the
    // minimum down rate (109008 / 38000 < 2.87 s). Total is 213648 bytes.
    const WARMUP: usize = 8;
    let tx: Vec<_> = (0..160).map(|serial| frame(MIC_FRAME_BYTES, serial, 83)).collect();
    let rx: Vec<_> = (0..WARMUP + 64).map(|serial| frame(watch::ETH_MAX, serial, 197)).collect();
    let mut r = Rig::new(tx.len() * MIC_FRAME_BYTES, APP_RX_SLOTS, Some(MIC_FRAME_BYTES), MIC_PERIOD_MS);
    r.boot();
    r.queue(&rx);
    let queued = r.machine.now();
    while r.rx.len() < WARMUP {
        r.step(true, true);
        assert!(r.machine.now() - queued < r.machine.tick_hz(), "picture warmup completes within one simulated second");
    }
    let started = r.machine.now();
    let first_tx = r.tx.len();
    let first_rx = r.rx.len();
    assert!(first_tx > 0 && r.state("sent") < tx.len() as u32, "microphone is active at the measurement start");
    while r.rx.len() < rx.len() {
        r.step(true, true);
        assert!(r.machine.now() - started < 4 * r.machine.tick_hz(), "picture transfer completes within four simulated seconds");
    }
    // Count both directions over the SAME settled simulated-time window.
    // The 700 B/s Ethernet headroom covers a boundary's one partial mic
    // frame; the separate retry and byte checks prove every 640-byte sample
    // block was sent at the offered 20 ms cadence without loss or backlog.
    let seconds = (r.machine.now() - started) as f64 / r.machine.tick_hz() as f64;
    let up = ((r.tx.len() - first_tx) * MIC_FRAME_BYTES) as f64 / seconds;
    let down = ((r.rx.len() - first_rx) * watch::ETH_MAX) as f64 / seconds;
    report_rates("app mix (settled overlap)", APP_RX_SLOTS, up, down);
    assert!(down >= SPEAKER_RATE as f64,
        "{APP_RX_SLOTS} slots give {down:.0} B/s down with {up:.0} B/s up; the speaker needs {SPEAKER_RATE} B/s down");
    assert!(up >= MIC_RATE as f64, "microphone requires {MIC_RATE} B/s; simulated up is {up:.0} B/s");
    assert!(r.state("sent") < tx.len() as u32, "microphone remains active through the last picture frame");
    r.finish(&tx, &rx);
    r.check_exchange(rx.len());
    assert_eq!(r.state("retries"), 0, "the paced microphone never accumulates send backpressure");
}

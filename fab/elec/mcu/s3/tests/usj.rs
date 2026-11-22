//! USB host polling and late attach on the model's frame grid.

use fab_elec_mcu_emit32::xtensa::enc;
use fab_elec_mcu_emu::{LinkOut, Machine, PcSet, Ran, Status};
use fab_elec_mcu_s3::{ResetScope, S3Machine};

mod support;
use support::*;

#[test]
fn host_listening_controls_tokens_packets_and_survives_core_reset() {
    // usb_serial_jtag_reg.h:69; TRM p.1259 §33.6: IN token for IN EP1.
    const IN_TOKEN: u32 = 1 << 8;
    // facts-machine.md §2: USB source 96, matrix status word 3 bit 0.
    const USB_MAP: u32 = 0x600c_2000 + 96 * 4;
    const STATUS3: u32 = 0x600c_2000 + 0x198;
    const PHASE: u64 = 48_000;
    const CLOSED_END: u64 = PHASE + 303 * USB_FRAME;
    const REOPEN_FRAME: u64 = CLOSED_END + USB_FRAME;

    // CCOUNT starts at a known tick; WAITI and a two-instruction handler
    // skip long waits without an instruction loop. The handler disables
    // INTENABLE through a15 = 0, then RFE resumes the fixture. Each new
    // CCOMPARE0 write clears the previous timer request (ISA pp.154,307).
    fn wait_until(p: &mut Program, count_start: u64, tick: u64) {
        p.sr(240, ((tick - count_start) / 24) as u32);
        p.sr(228, 1 << 6);
        p.emit(enc::rrr(0, 0, 0, 7, 0, 0)); // WAITI 0.
    }

    // RAW and CLR must both clear bit 8, including a same-tick poll.
    for clear in [USB_CLR, USB_RAW] {
        let mut p = Program::new();
        p.disable_watchdogs();
        p.sr(231, IRAM);
        p.sr(230, 0);
        p.sr(228, 0);
        p.emit(enc::movi(15, 0));
        p.store(USB_MAP, 0);
        p.store(USB_ENA, IN_TOKEN);
        let count_start = access_tick(p.sr(234, 0));
        p.usb_tx_sequence(0, 64); // Full packet auto-flushes, no WR_DONE.
        p.pin_read(7, USB_EP1_CONF);
        let pending = p.pc();
        wait_until(&mut p, count_start, PHASE);
        p.pin_read(6, USB_RAW);
        p.pin_read(7, USB_EP1_CONF);
        p.pin_read(8, USB_ST);
        p.pin_read(9, STATUS3);
        let consumed = p.pc();
        p.store(clear, IN_TOKEN);
        p.pin_read(6, USB_RAW);
        let cleared = p.pc();
        // Two handler instructions and the store's two literal loads put
        // its S32I exactly at the second frame, after that frame's event.
        wait_until(&mut p, count_start, PHASE + USB_FRAME - 4 * 24);
        p.store(clear, IN_TOKEN);
        p.pin_read(6, USB_RAW);
        let race_cleared = p.pc();
        wait_until(&mut p, count_start, PHASE + 2 * USB_FRAME);
        p.pin_read(6, USB_RAW);
        let idle_polled = p.pc();
        p.usb_tx_sequence(64, 64);
        p.store(clear, IN_TOKEN);
        p.pin_read(6, USB_RAW);
        p.pin_read(7, USB_EP1_CONF);
        let stalled = p.pc();
        wait_until(&mut p, count_start, CLOSED_END);
        p.pin_read(6, USB_RAW);
        p.pin_read(7, USB_EP1_CONF);
        p.pin_read(8, USB_ST);
        p.pin_read(9, STATUS3);
        let closed_checked = p.pc();
        wait_until(&mut p, count_start, REOPEN_FRAME);
        p.pin_read(6, USB_RAW);
        p.pin_read(7, USB_EP1_CONF);
        p.pin_read(8, USB_ST);
        p.pin_read(9, STATUS3);
        p.usb_rx_words(RAM, 3);
        let reopened = p.pc();
        p.store(RTC, 1 << 31); // TRM §7.1: software Core reset.

        let mut board = profile(123);
        board.pins.usb_frame_phase_ticks = PHASE;
        let mut m = S3Machine::new(p.finish(), &board);
        let handler: Vec<u8> = [enc::rsr(0, 3, 1, 228, 15), enc::rrr(0, 0, 0, 3, 0, 0)]
            .into_iter().flat_map(|insn| insn.to_le_bytes().into_iter().take(3)).collect();
        assert!(m.seed_data(RAM + 0x300, &handler), "timer handler is seeded before execution");
        let marks = PcSet::default();
        let mut out = Vec::new();

        pins_at_pc(&mut m, pending);
        assert!(m.now() < PHASE, "auto-flush precedes the first host frame");
        assert_eq!(m.cpu().ar[7] & 6, 0, "64-byte auto-flush holds DATA_FREE low until consumption");
        m.take_out(&mut out);
        assert!(out.is_empty(), "auto-flushed bytes are not delivered before a host poll");
        pins_at_pc(&mut m, consumed);
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, IN_TOKEN, "default listening latches an IN token at the first frame");
        assert_eq!(m.cpu().ar[7] & 6, 2, "first frame frees the consumed IN packet");
        assert_eq!(m.cpu().ar[8] & IN_TOKEN, IN_TOKEN, "enabled token appears in USB ST");
        assert_eq!(m.cpu().ar[9] & 1, 1, "enabled token asserts USB source 96");
        assert_eq!(m.cpu().interrupt & 1, 1, "USB source 96 reaches the routed CPU line");
        m.take_out(&mut out);
        assert_eq!(out, (0..64).map(|byte| LinkOut { link: 0, tick: PHASE, byte }).collect::<Vec<_>>(),
            "default host consumes all 64 bytes at the next grid frame");
        out.clear();
        pins_at_pc(&mut m, cleared);
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, 0, "RAW or CLR clears an older IN token");
        assert_eq!(m.cpu().interrupt & 1, 0, "clearing the only enabled USB cause drops the CPU line");
        assert_eq!(run_to(&mut m, PHASE + USB_FRAME, &marks), Ran::Tick, "idle host reaches its next poll");
        assert_eq!(m.cpu().interrupt & 1, 1, "next frame reasserts the token with no flushed packet");
        pins_at_pc(&mut m, race_cleared);
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, 0, "same-tick RAW or CLR loses the new IN token");
        let mut notes = Vec::new();
        m.take_notes(&mut notes);
        assert!(notes.iter().any(|n| n.tick == PHASE + USB_FRAME && n.what.contains("event lost")),
            "same-tick token loss records the existing priority assumption");
        pins_at_pc(&mut m, idle_polled);
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, IN_TOKEN, "idle polls continue on the third frame after a lost token");

        let closed_at = m.now();
        m.set_usb_host_listening(false);
        for byte in [0xa1, 0xb2, 0xc3] { m.link_in(0, byte); }
        pins_at_pc(&mut m, stalled);
        let stalled_at = m.now();
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, 0, "token can be cleared after closing the host port");
        assert_eq!(m.cpu().ar[7] & 6, 0, "closed host leaves IN busy and queued OUT unavailable");
        assert!(CLOSED_END - stalled_at >= 300 * USB_FRAME, "closed-port observation spans at least 300 ms");
        assert_eq!(run_to(&mut m, CLOSED_END - 1, &marks), Ran::Tick, "closed host permits a long simulated sleep");
        assert_eq!(m.status(), Status::Sleeping { wake: Some(CLOSED_END) }, "closed host schedules no intervening USB frame events");
        pins_at_pc(&mut m, closed_checked);
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, 0, "no IN token arrives during 300 ms with the port closed");
        assert_eq!(m.cpu().ar[7] & 6, 0, "flushed IN stays unread and link_in bytes stay queued while closed");
        assert_eq!(m.cpu().ar[8] & IN_TOKEN, 0, "closed host leaves token ST clear");
        assert_eq!(m.cpu().ar[9] & 1, 0, "closed host leaves USB source 96 deasserted");
        m.take_out(&mut out);
        assert!(out.is_empty(), "closed port delivers no flushed IN bytes");
        notes.clear();
        m.take_notes(&mut notes);
        assert!(notes.iter().any(|n| n.tick == closed_at && n.what.contains("closed port polls EP1")),
            "closing the host records the open hardware question as a model assumption");

        let opened_at = m.now();
        m.set_usb_host_listening(true);
        assert!(REOPEN_FRAME > opened_at && REOPEN_FRAME - opened_at <= USB_FRAME,
            "reopening schedules the next existing grid frame within one millisecond");
        assert_eq!(run_to(&mut m, REOPEN_FRAME - 1, &marks), Ran::Tick, "reopened host waits for the grid");
        assert_eq!(m.cpu().interrupt & 1, 0, "reopening alone does not synthesize an early token");
        m.take_out(&mut out);
        assert!(out.is_empty(), "reopening alone does not consume IN before the frame");
        pins_at_pc(&mut m, reopened);
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, IN_TOKEN, "first reopened frame latches an IN token");
        assert_eq!(m.cpu().ar[7] & 6, 6, "reopened frame frees IN and delivers queued OUT");
        assert_eq!(m.cpu().ar[8] & IN_TOKEN, IN_TOKEN, "reopened token reaches enabled USB ST");
        assert_eq!(m.cpu().ar[9] & 1, 1, "reopened token asserts USB source 96");
        assert_eq!(m.cpu().interrupt & 1, 1, "reopened token asserts the routed CPU line");
        m.take_out(&mut out);
        assert_eq!(out, (64..128).map(|byte| LinkOut { link: 0, tick: REOPEN_FRAME, byte }).collect::<Vec<_>>(),
            "reopened host consumes the stalled IN packet once at the next frame");
        assert_eq!(received_words(&m, RAM, 3), [0xa1, 0xb2, 0xc3], "closed-port OUT bytes arrive in their original order");
        out.clear();

        m.set_usb_host_listening(false);
        m.link_in(0, 0xdd);
        assert!(matches!(run_to(&mut m, REOPEN_FRAME + USB_FRAME, &marks), Ran::Reset { .. }),
            "firmware performs a Core reset while the host is closed");
        assert_eq!(m.last_reset().map(|r| r.scope), Some(ResetScope::Core), "reset exercises the digital-system scope");
        pins_at_pc(&mut m, consumed);
        assert_eq!(m.cpu().ar[6] & IN_TOKEN, 0, "Core reset retains the closed host through the next frame");
        assert_eq!(m.cpu().ar[7] & 6, 0, "Core reset does not reopen IN consumption or queued OUT delivery");
        assert_eq!(m.cpu().ar[9] & 1, 0, "Core reset leaves the closed host's USB source inactive");
        m.take_out(&mut out);
        assert!(out.is_empty(), "host remains closed after Core reset despite firmware auto-flush");
    }
}

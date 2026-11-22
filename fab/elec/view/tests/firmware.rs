use fab_elec_bench::{BenchSpec, Firmware};
use fab_elec_core::{BoardId, DocId};
use fab_elec_design::TextEdit;
use fab_elec_lang::{BenchEdit, BenchOut, BenchScope, BenchView, UnitState};
use fab_elec_mcu_cosim::{net, watch, At, FaultCount, FwCondCount, FwFault, FwPin, FwState, NetConn, NetStatus, StepMap};
use fab_elec_mcu_emu::{FwCond, FwDebug, FwHandler, FwStmt, FwTimer, FwTy, FwValue, FwVar, Halt, HaltKind, Pending, PinUse, TimerEvent, Wake};
use fab_elec_sim::cosim::{CoState, StepTo};
use fab_elec_view::fw_state::{cond_marks, line_at, marker, ran_lines, state_columns, state_text, unrun_lines, CondMark, Heat, Mark, SourceLines, StateText, HEAT_FADE, HEAT_PEAK, SOURCE_MIN, STATE_PANEL_TWO};
use fab_elec_view::firmware::{firmware_status, shows_folded, typing_label, FirmwareNetToken, FirmwareShown, FirmwareUpdate, FOLD_UNDER, TYPING_PAUSE};
use fab_elec_view::live::fw_steps::{fold_to_icons, steps, Steps};
use fab_elec_view::live::sim::{FlashLive, FlashState, FwDiagnostic, FwLive, MonitorState, NetServerLive, NetworkLive, RadioLive, RadioState, TokenLive};
use fab_elec_view::snapshot::{DocText, DocsView, Refusal, ViewAction};
use std::sync::Arc;

#[test]
fn radio_face_follows_the_pack() {
    let face = |state| fab_elec_view::fw_state::radio_face(&RadioLive { state, total_bytes: 3_820_124 });
    assert_eq!(face(RadioState::Absent), "Wi-Fi libraries (Espressif, Apache-2.0, 3.8 MB): not installed",
        "before Get the row identifies the download, license and size");
    assert_eq!(face(RadioState::Fetching { done: 1_570_000, file: "esp32s3/libpp.a".into() }),
        "getting 41%: esp32s3/libpp.a", "progress names the current file and percentage of the whole pack");
    assert_eq!(face(RadioState::Installing), "installing", "installation has its own status");
    assert_eq!(face(RadioState::Linking), "linking", "linking has its own status");
    assert_eq!(face(RadioState::Ready { island_bytes: 332_900 }),
        "ready: radio island 333 KB; the simulator answers its calls with Fab's stand-in, and Espressif's code runs only on the board",
        "ready shows the linked image's size");
    assert_eq!(face(RadioState::Failed("missing LICENSE".into())), "failed: missing LICENSE", "failure keeps its reason");
    assert_eq!(face(RadioState::Cancelled), "cancelled", "cancellation is distinct from failure");
}

fn docs(rev: u64, text: &str) -> Arc<DocsView> {
    Arc::new(DocsView {
        texts: vec![
            DocText { doc: DocId(0), name: "ledavr.ckt.splash".into(), text: "circuit".into(), rev: 1 },
            DocText { doc: DocId(1), name: "ledavr.fw.splash".into(), text: text.into(), rev },
        ],
        ..DocsView::default()
    })
}

fn bound(f: impl FnOnce(&mut FwLive)) -> Arc<FwLive> {
    let mut fw = FwLive { part: "u2".into(), doc: "ledavr.fw.splash".into(), ..FwLive::default() };
    f(&mut fw);
    Arc::new(fw)
}

/// A bench in `state` binding the firmware document `doc` (none when empty).
fn bench(state: UnitState, doc: &str) -> BenchView {
    let firmware = if doc.is_empty() { vec![] } else { vec![Firmware { path: "mcu.u".into(), doc: doc.into(), params: vec![] }] };
    BenchView {
        out: BenchOut {
            name: "chase".into(), doc: DocId(0), scope: BenchScope::Design(BoardId(0)),
            spec: BenchSpec { firmware, ..BenchSpec::default() },
            expects: vec![], probes: vec![], stop: None, live: false, panel: None, state,
        },
        edits: vec![],
        insert: BenchEdit::Literal,
    }
}

fn error(message: &str) -> FwDiagnostic {
    FwDiagnostic { span: Some(0..1), param: None, message: message.into() }
}

#[test]
fn the_firmware_pane_shows_the_bound_document_and_how_its_build_stands() {
    let mut shown = FirmwareShown::default();
    let none = Arc::new(vec![bench(UnitState::Built, "")]);
    let first = shown.update(None, &none, None, None).expect("the first look shows something");
    assert!(first.empty.is_some() && first.load.is_none(), "a bench without firmware says so and loads no code");
    assert!(shown.update(None, &none, None, None).is_none(), "nothing changed: no work");

    // Before the run is made, the bench it will run (the first that evaluated) names the document.
    let text = docs(3, "loop {}");
    let benches = Arc::new(vec![bench(UnitState::Failed(vec![]), "other.fw.splash"), bench(UnitState::Built, "ledavr.fw.splash")]);
    let up = shown.update(None, &benches, Some(&text), None).expect("the benches changed");
    assert_eq!((up.title.as_str(), up.status.as_str(), up.empty), ("ledavr.fw.splash", "compiles when the bench runs", None),
        "the run's bench shows its firmware before anything compiles, not a failed bench's");
    assert_eq!(up.load.as_ref().map(|(name, text)| (name.as_str(), &**text)), Some(("ledavr.fw.splash", "loop {}")),
        "its text loads under its own name, which picks the highlighting, not the circuit's beside it");

    let fw = bound(|fw| fw.compiling_rev = Some(3));
    let up = shown.update(Some(&fw), &benches, Some(&text), None).expect("a bound firmware shows");
    assert_eq!((up.title.as_str(), up.status.as_str(), up.load), ("ledavr.fw.splash", "compiling…", None),
        "the run's own status takes over, and the code it already shows stays");

    let running = bound(|fw| fw.running_rev = Some(3));
    let up = shown.update(Some(&running), &benches, Some(&text), None).expect("the status changed");
    assert_eq!((up.status.as_str(), up.load), ("running", None), "a status change alone does not reload the code");

    let edited = docs(4, "loop { wait() }");
    let up = shown.update(Some(&running), &benches, Some(&edited), None).expect("the documents changed");
    assert_eq!(up.status, "the last build runs", "the text moved past the running build");
    assert!(up.load.is_some_and(|(_, text)| &*text == "loop { wait() }"), "a new revision reloads the code");
    let same_rev = docs(4, "loop { wait() }");
    assert!(shown.update(Some(&running), &benches, Some(&same_rev), None).is_some_and(|up| up.load.is_none()),
        "new documents at the same revision keep the editor as it is");

    let failed = bound(|fw| { fw.running_rev = Some(3); fw.failed = Some((4, vec![error("unknown pin PD9"), error("x")])); });
    assert_eq!(firmware_status(&failed, Some(4)), "does not compile: unknown pin PD9 (and 1 more) · the last build runs",
        "a failed edit names its first error and says the earlier build still runs");
    let fixed = bound(|fw| { fw.running_rev = Some(5); fw.failed = Some((4, vec![error("unknown pin PD9")])); });
    assert_eq!(firmware_status(&fixed, Some(5)), "running", "a failure older than the running build is history");
    let missing = bound(|fw| fw.problem = Some("the bench names no clock".into()));
    assert_eq!(firmware_status(&missing, None), "the bench names no clock", "a binding that cannot compile says why");
    for (state, said) in [
        (FlashState::Flashing("writing 2/2: 40%".into()), "running · flashing: writing 2/2: 40%"),
        (FlashState::Failed("the board is gone".into()), "running · flash failed: the board is gone"),
        (FlashState::Reading("the whole flash's MD5".into()), "running · reading the board: the whole flash's MD5"),
        (FlashState::ReadFailed("ESP ROM reply timed out".into()), "running · read failed: ESP ROM reply timed out"),
        (FlashState::BackingUp("3 of 16 MB, 10 min left".into()), "running · backing up the board: 3 of 16 MB, 10 min left"),
        (FlashState::BackupFailed("the read was stopped".into()), "running · back-up failed: the read was stopped"),
        (FlashState::Idle, "running"),
    ] {
        let flashing = bound(|fw| { fw.running_rev = Some(5); fw.flash = Some(FlashLive { state, ..FlashLive::default() }); });
        assert_eq!(firmware_status(&flashing, Some(5)), said, "the status says how a flash goes, after how the build stands");
    }
    for (monitor, said) in [
        (MonitorState::Starting, "running · monitor: restarting the board"),
        (MonitorState::On, "running · monitoring"),
        (MonitorState::Paused, "running · monitor paused"),
        (MonitorState::Ended("the board went away".into()), "running · monitor ended: the board went away"),
    ] {
        let watching = bound(|fw| { fw.running_rev = Some(5); fw.flash = Some(FlashLive { monitor, ..FlashLive::default() }); });
        assert_eq!(firmware_status(&watching, Some(5)), said, "the status says how the monitor stands");
    }
    let offered = bound(|fw| { fw.running_rev = Some(5); fw.flash = Some(FlashLive::default()); });
    assert!(shown.update(Some(&offered), &benches, Some(&same_rev), None).is_some_and(|up| up.flash == offered.flash),
        "the pane offers Flash to while the running image can be flashed");

    assert!(shows_folded(FOLD_UNDER - 1.0) && !shows_folded(FOLD_UNDER), "lower than the fold line the pane shows only its head");

    let gone = Arc::new(DocsView::default());
    let up = shown.update(Some(&running), &benches, Some(&gone), None).expect("the documents changed");
    assert!(up.empty.is_some_and(|why| why.contains("ledavr.fw.splash")) && up.load.is_none(),
        "a bound document missing from the project is named in place of the code");
}

/// The edit a typed change sends: (base revision, the replacement, its undo group, its tag).
fn sent(action: Option<ViewAction>) -> (u64, TextEdit, u64, u64) {
    match action {
        Some(ViewAction::Text { base_rev, mut edits, group: Some(group), label, tag, .. }) if edits.len() == 1 => {
            assert_eq!(label, typing_label("ledavr.fw.splash"), "the pane's typing is named for its document");
            (base_rev, edits.remove(0), group, tag)
        }
        other => panic!("a typed change sends one replacement in an undo group: {other:?}"),
    }
}

fn refusal(seq: u64, op: &'static str, tag: u64) -> Arc<Refusal> {
    Arc::new(Refusal { seq, label: typing_label("ledavr.fw.splash"), op, text: "refused".into(), reason: "refused".into(), site: None, sample: Some(tag) })
}

#[test]
fn the_firmware_pane_sends_one_typed_edit_at_a_time_and_never_loses_typing() {
    let mut shown = FirmwareShown::default();
    let benches = Arc::new(vec![bench(UnitState::Built, "ledavr.fw.splash")]);
    let look = |shown: &mut FirmwareShown, docs: &Arc<DocsView>, refusal: Option<&Arc<Refusal>>| {
        shown.update(None, &benches, Some(docs), refusal).expect("the documents or the refusal changed")
    };
    let loaded = |up: FirmwareUpdate| up.load.map(|(_, text)| text.to_string());
    assert!(shown.typed(0.0, || "x".into()).is_none(), "nothing loaded: nothing to send");
    assert_eq!(loaded(look(&mut shown, &docs(3, "loop {}"), None)), Some("loop {}".into()), "the document loads");
    assert!(shown.typed(0.0, || "loop {}".into()).is_none(), "an unchanged text sends nothing");

    // One edit on the way; a change meanwhile goes once the first shows applied, in its group.
    let (base, edit, group, first) = sent(shown.typed(0.1, || "loop { a }".into()));
    assert_eq!((base, edit), (3, TextEdit { doc: DocId(1), span: 6..6, text: " a ".into() }),
        "the change addresses the revision the pane read, as the smallest replacement");
    assert!(shown.typed(0.2, || "loop { ab }".into()).is_none() && !shown.sends(), "a change while one is on the way waits");
    assert_eq!(loaded(look(&mut shown, &docs(3, "loop {}"), None)), None, "documents from before the edit applied keep the editor");
    assert!(!shown.sends(), "the waiting change does not go before the first shows applied");
    assert_eq!(loaded(look(&mut shown, &docs(4, "loop { a }"), None)), None, "the edit applied: the editor holds that text");
    assert!(shown.sends(), "the waiting change goes now");
    let (base, edit, same, second) = sent(shown.typed(0.3, || "loop { ab }".into()));
    assert_eq!((base, edit.span, edit.text.as_str(), same), (4, 8..8, "b", group), "it addresses the first one's revision, in its group");
    assert_ne!(second, first, "each edit has its own tag");
    assert_eq!(loaded(look(&mut shown, &docs(5, "loop { ab }"), None)), None, "the second applied");

    // Another writer changed another part first: the typing moves onto the new text and goes again.
    let (_, _, later, _) = sent(shown.typed(0.3 + 2.0 * TYPING_PAUSE, || "loop { abc }".into()));
    assert_ne!(later, group, "a change after a pause starts another undo step");
    let up = look(&mut shown, &docs(6, "// note\nloop { ab }"), None);
    assert_eq!((loaded(up.clone()), up.kept), (Some("// note\nloop { abc }".into()), false),
        "the editor shows the other writer's change with the typing in it");
    let (base, edit, _, moved) = sent(shown.typed(10.0, || "// note\nloop { abc }".into()));
    assert_eq!((base, edit.span, edit.text.as_str()), (6, 17..17, "c"), "the moved typing goes on the new revision");

    // A refusal is the pane's by kind and tag, whatever its wording.
    for (seq, op, tag) in [(1, "text", moved + 1000), (2, "board", moved)] {
        let up = look(&mut shown, &docs(6, "// note\nloop { ab }"), Some(&refusal(seq, op, tag)));
        assert!(up.load.is_none() && !up.kept, "another edit's refusal ({op}, tag {tag}) keeps the editor as it is");
    }
    // Refused with the document as it was: the typing is kept, and the pane says so.
    let up = look(&mut shown, &docs(6, "// note\nloop { ab }"), Some(&refusal(3, "text", moved)));
    assert!(up.load.is_none() && up.kept && up.status.starts_with("Not in the document: refused"),
        "the pane's refused typing stays in the editor, kept, and the pane says why: {up:?}");
    assert!(!shown.sends() && shown.typed(10.5, || "// note\nloop { abc }".into()).is_none(), "kept typing waits for the person");
    assert_eq!(shown.take_document().map(|(_, text)| text.to_string()), Some("// note\nloop { ab }".into()),
        "taking the document's text loads it in place of the typing");
    let up = look(&mut shown, &docs(6, "// note\nloop { ab }"), Some(&refusal(3, "text", moved)));
    assert!(!up.kept && up.load.is_none() && !shown.sends(), "nothing is kept or waits after taking the document's text");

    // Another writer changed the same place: the typing is kept until the person keeps it.
    let (base, ..) = sent(shown.typed(20.0, || "// note\nloop { x }".into()));
    assert_eq!(base, 6, "typing goes on from the document's revision");
    let up = look(&mut shown, &docs(7, "// note\nloop { y }"), None);
    assert!(up.load.is_none() && up.kept && up.status.contains("ledavr.fw.splash changed where you typed"),
        "where the changes meet, the editor keeps the typing and the pane names the document: {up:?}");
    let (base, edit, ..) = sent(shown.keep_mine(21.0));
    assert_eq!((base, edit.span, edit.text.as_str()), (7, 15..16, "x"), "kept typing goes over the document's text as it is");
    let running = bound(|fw| fw.running_rev = Some(8));
    let up = shown.update(Some(&running), &benches, Some(&docs(8, "// note\nloop { x }")), None).expect("the documents changed");
    assert_eq!((up.load, up.kept, up.status.as_str()), (None, false, "running"), "the kept typing applied and runs");
}

/// A device stopped by a step before `tick`'s line 7 (`setup`'s line 2 began last): two variables, a PWM pin, an output and a
/// pulled-up input, Timer1 in fast PWM, a pending compare interrupt, a timer fault three times.
fn device(f: impl FnOnce(&mut FwState)) -> FwState {
    let debug = FwDebug {
        handlers: vec![FwHandler { name: "setup".into(), span: (0, 10) }, FwHandler { name: "tick".into(), span: (11, 90) }],
        stmts: vec![
            FwStmt { addr: Some(0x40), span: (2, 8), line: 2, col: 5, owner: fab_elec_mcu_emu::Owner::Handler(0) },
            FwStmt { addr: Some(0x52), span: (20, 31), line: 7, col: 5, owner: fab_elec_mcu_emu::Owner::Handler(1) },
        ],
        starts: vec![(0x40, vec![0]), (0x52, vec![1])],
        vars: vec![FwVar { name: "count".into(), ty: FwTy::I32, addr: 0x100, span: (0, 0) }, FwVar { name: "level".into(), ty: FwTy::F32, addr: 0x104, span: (0, 0) }],
        ..FwDebug::default()
    };
    let fault = FwFault { cycle: 19_000, code: watch::FAULT_TIMER, site: 2, arg: 11 };
    let mut s = FwState {
        chain: Vec::new(),
        debug: Some(Arc::new(debug)), image_id: 7, source_rev: 3, t: 1.25e-3, cycle: 20_000, t_reset: 0.0, f_clk: 16e6,
        at: At::Statement(0), handler: Some(0), last: Some(0), ran: vec![(0, 1)], ran_counts: vec![(0, 1)], cond_counts: vec![],
        halt_detail: None,
        vars: vec![FwValue::I32(3), FwValue::F32(0.5)],
        fault_cells: vec![],
        pins: vec![
            FwPin { pin: "PB1", mode: PinUse::Pwm, level: Some(true), duty: 0.25 },
            FwPin { pin: "PB0", mode: PinUse::Output, level: Some(true), duty: 0.0 },
            FwPin { pin: "PD2", mode: PinUse::PullUp, level: None, duty: 0.0 },
        ],
        timers: vec![FwTimer {
            name: "Timer1", mode: "fast pwm", prescale: 8, top: 255, count: 10, compares: vec![64],
            next: Some((TimerEvent::Compare(0), 20_100)),
        }],
        pending: vec![Pending::Irq { source: "TIMER1_COMPA", vector: 11 }],
        wake: None,
        faults: vec![],
        fault_counts: vec![FaultCount { code: watch::FAULT_TIMER, site: 2, count: 3, last: fault }],
        radio_starts: Default::default(),
        radio_illegal_pc: None,
        stopped: Some(StepTo::Statement),
        pc: 0x52,
        regs: vec![("r0", 0), ("r1", 1), ("SREG", 0x80), ("SP", 0x08ff)],
        eeprom: None,
        net: None,
        net_log: None,
    };
    f(&mut s);
    s
}

fn rows(text: &StateText) -> Vec<(&str, &str, Option<bool>)> {
    text.rows.iter().map(|row| (row.name.as_str(), row.value.as_str(), row.truth)).collect()
}

/// The state panel's text is the device's state, value by value (what `elec.fw.state` reads):
/// where it stands, variables, pins, timers, pending work, the fault tally and registers.
#[test]
fn the_state_panel_reads_each_value_of_the_device_state() {
    let text = state_text(&device(|_| {}));
    assert_eq!(text.at, "Stopped before tick, line 7", "a step's stop names the statement about to execute: its handler and line");
    assert_eq!(text.head, "AT THE PLAYHEAD", "the heading names the instant without its time (the recorder shows it)");
    assert_eq!(rows(&text), [("count", "3", None), ("level", "0.5", None), ("PC", "0x0052", None), ("cycle", "20000", None)],
        "the variables in table order, then the PC and cycle");
    assert_eq!(text.pins, ["PB1  PWM 25 %, high", "PB0  out, high", "PD2  in, pull-up, ?"],
        "each pin's use and level, a PWM pin's duty too (its share of time high) beside its level at the instant");
    assert_eq!(text.timers, ["Timer1  fast pwm, clk/8", "  count 10 of 255, compare 64", "  next: compare A in 100 cycles"],
        "a timer's mode and clock, count and compares, and its next event from the cycle");
    assert_eq!(text.pending, ["TIMER1_COMPA (vector 11)"], "the pending interrupt");
    assert_eq!(text.faults, ["timer fault at site 2 \u{d7}3, last at cycle 19000 (arg 11)"], "the tally, with its latest report");
    assert_eq!(text.regs, ["r0 00  r1 01  SREG 80  SP 08ff"], "the registers four to a line");

    let report = state_text(&device(|s| {
        for (code, arg) in [(watch::FAULT_KERNEL_TIMER_REFUSED, 7), (watch::FAULT_KERNEL_CRIT_MISMATCH, 0x4037_8a10),
            (watch::FAULT_KERNEL_CRIT_SPAN, 912), (watch::FAULT_KERNEL_BLOCK_IN_CRITICAL, 3),
            (watch::FAULT_KERNEL_IRQ_LINE_REFUSED, 31), (watch::FAULT_KERNEL_ISR_ENTRY_REFUSED, 0x4038_0000),
            (watch::FAULT_RADIO_ISLAND_MISMATCH, 0x9e3d_51c7_u32 as i32), (watch::FAULT_KERNEL_TIMER_LOST, 0x3fc9_1a20),
            (watch::FAULT_KERNEL_TIMER_UNSET, 0x3fc9_1a40), (watch::FAULT_KERNEL_HEAP_EXHAUSTED, 96),
            (watch::FAULT_KERNEL_TIMER_HANDLE_INVALID, 0x3fc9_1a60), (watch::FAULT_RADIO_PHY_TABLE_REFUSED, 0x3fce_fd74)] {
            let last = FwFault { cycle: 19_000, code, site: 2, arg };
            s.radio_starts.observe(&last);
            s.fault_counts.push(FaultCount { code: last.code, site: last.site, count: 3, last });
        }
        let last = FwFault { cycle: 19_000, code: watch::FAULT_KERNEL_ISR_DEPTH, site: watch::SITE_RADIO_SCHEDULER, arg: 2 };
        s.fault_counts.push(FaultCount { code: last.code, site: last.site, count: 1, last });
    }));
    assert_eq!(report.faults, [
        "radio stopped: a radio timer was lost or overwritten; it restarts by itself",
        "timer fault at site 2 \u{d7}3, last at cycle 19000 (arg 11)",
        "radio timer refused (no radio memory; it stays pending): 7 live timers at site 2 \u{d7}4\u{2013}7, last at cycle 19000",
        "radio critical section mismatch: return 0x40378a10 at site 2 \u{d7}3, last at cycle 19000",
        "radio masked span: 912 cycles at site 2 \u{d7}3, last at cycle 19000",
        "radio export blocked in a critical section at site 2 \u{d7}3, last at cycle 19000 (export 3)",
        "radio interrupt line 31 refused (runtime-owned, not level 1, or out of range) at site 2 \u{d7}3, last at cycle 19000",
        "radio ISR entry 0x40380000 outside the island code region at site 2 \u{d7}3, last at cycle 19000",
        "radio island missing or doesn't match this firmware (header CRC 0x9e3d51c7); the radio stays off at site 2 \u{d7}3, last at cycle 19000",
        "radio timer 0x3fc91a20 lost (no radio memory when armed; it won't fire) at site 2 \u{d7}4\u{2013}7, last at cycle 19000",
        "radio timer 0x3fc91a40 armed before it was set (ignored) at site 2 \u{d7}4\u{2013}7, last at cycle 19000",
        "radio memory refused 96 bytes at site 2 \u{d7}4\u{2013}7, last at cycle 19000",
        "radio timer 0x3fc91a60 overwritten by the radio's code (refused) at site 2 \u{d7}4\u{2013}7, last at cycle 19000",
        "radio PHY function table at 0x3fcefd74 is outside ROM data RAM; restarts won't restore it at site 2 \u{d7}3, last at cycle 19000",
        "radio ISR returned at depth 2 in the radio scheduler, last at cycle 19000",
    ], "a report shows its argument as its code defines it before its site and tally; a doubling code's tally of n \
        reports reads as 2^(n-1) to 2^n - 1 failures; a lost or overwritten timer puts the degraded line first; a blocked \
        export names the export; the scheduler's reports name it, not a site; other fault lines stay unchanged");

    let asleep = state_text(&device(|s| {
        s.at = At::Sleeping;
        s.stopped = None;
        s.wake = Some(Wake { tick: 24_000, source: "INT0", vector: 1 });
    }));
    assert_eq!(asleep.at, "Sleeping until INT0 at 1.50 ms", "a sleeping device names its wake and when");
    let halted = state_text(&device(|s| {
        s.at = At::Halted(Halt { tick: 19_990, pc: 0x60, kind: HaltKind::Break, what: "break" });
        s.stopped = None;
    }));
    assert_eq!(halted.at, "Halted at 0x0060: a break (break)", "a halt names its address and cause");
    let mut unmodeled = device(|s| {
        s.at = At::Halted(Halt { tick: 19_990, pc: 0x4037_8003, kind: HaltKind::Unmodeled, what: "foreign body rule" });
        s.stopped = None;
    });
    assert_eq!(state_text(&unmodeled).at, "Halted at 0x40378003: not modelled: foreign body rule");
    unmodeled.halt_detail = Some("esp_wifi_start+0x3, step 1: the start mutex was not acquired".into());
    assert_eq!(state_text(&unmodeled).at,
        "Halted at 0x40378003: not modelled: esp_wifi_start+0x3, step 1: the start mutex was not acquired");
    let bare = state_text(&device(|s| { s.debug = None; s.pc = 0x01a6; }));
    assert_eq!(bare.at, "Stopped: statement 0", "without a table the stop names the statement id");
    assert_eq!(rows(&bare), [("PC", "0x01A6", None), ("cycle", "20000", None)], "without variables the device still shows its PC and cycle");
    let idle = StateText::idle();
    assert_eq!(idle.head, "AT THE PLAYHEAD", "the idle heading has no time");
    assert!(!idle.at.is_empty() && idle.rows.is_empty() && idle.pins.is_empty() && idle.regs.is_empty(),
        "before the bench runs firmware the panel says so and shows no values");
}

#[test]
fn the_network_row_and_the_state_panel_say_what_the_twin_reaches() {
    use fab_elec_view::fw_state::{network_face, network_hosts, network_servers};
    let server = |c, name: Option<&str>, host: &str, approved, token| NetServerLive {
        c, name: name.map(str::to_string), host: host.into(), address: format!("wss://{host}:443/v1"), approved, token,
    };
    let mut row = NetworkLive {
        servers: vec![server(1, Some("svc"), "svc.example", true, TokenLive::Saved),
            server(2, None, "svc.example", true, TokenLive::NoName),
            server(3, Some("img"), "img.example", false, TokenLive::OtherHost("old.example".into()))],
        ..NetworkLive::default()
    };
    assert_eq!(network_hosts(&row), ["svc.example", "img.example"], "each host once, in the bench's order");
    let off = network_face(&row);
    assert!(off.contains("fakes") && off.contains("svc.example") && off.contains("img.example"),
        "off, the row says the fakes play and which hosts the real network would reach: {off}");
    row.on = true;
    row.last_used = Some("2026-11-19".into());
    let on = network_face(&row);
    assert!(on.contains("svc.example") && on.contains("img.example not allowed") && on.contains("2026-11-19"),
        "on, the row names the allowed hosts, the one not allowed, and the last use: {on}");
    row.held = Some("this window is hidden".into());
    assert!(network_face(&row).contains("this window is hidden"), "a held network says why whatever the choice");
    let servers = network_servers(&row);
    assert_eq!(servers.len(), 3, "a line per server");
    assert!(servers[2].contains("not allowed") && servers[2].contains("old.example"),
        "a server's line says its host is not allowed and that its token was given for another host: {}", servers[2]);

    let token = FirmwareNetToken::save("svc".into(), "svc.example".into(), b"s3cret-t0ken".to_vec());
    assert_eq!(token.token(), Some(&b"s3cret-t0ken"[..]));
    let shown = format!("{token:?}");
    assert!(!shown.contains("s3cret") && shown.contains("svc.example"), "an action's Debug never shows the token: {shown}");
    assert_eq!(FirmwareNetToken::forget("svc".into(), "svc.example".into()).token(), None);

    let state = device(|s| s.net = Some(NetStatus {
        link: Some((net::LINK_UP, net::code::NONE, 0)),
        conns: vec![NetConn { c: 0, state: net::READY, code: net::code::NONE, detail: 0 },
            NetConn { c: 1, state: net::OFFLINE, code: net::code::RESET, detail: 0 }],
        up: 5, down: 7, bytes_up: 20, bytes_down: 40, refused: 1, last_refusal: Some(net::HostError::OverGrant { c: 0 }),
        owner: true, cuts: 2,
    }));
    let lines = state_text(&state).net;
    assert!(lines[0].contains("up"), "the link: {lines:?}");
    assert!(lines[1].contains("c0") && lines[1].contains("ready"), "a ready connection: {lines:?}");
    assert!(lines[2].contains("c1") && lines[2].contains(net::code_text(net::code::RESET)), "an offline one with why: {lines:?}");
    assert!(lines.iter().any(|line| line.contains("OverGrant")), "a refusal names itself: {lines:?}");
    assert!(lines.iter().any(|line| line.contains("2 times")), "a run that went back and cut its log says so: {lines:?}");
    assert!(state_text(&device(|s| s.net = None)).net.is_empty(), "an image without network ports has no section");
}

#[test]
fn the_state_panel_names_start_failures_and_crypto_stubs() {
    for (step, name) in [(1, "init"), (2, "set mode"), (3, "start")] {
        let state = device(|s| {
            let last = FwFault { cycle: 20, code: watch::FAULT_RADIO_START_FAILED,
                site: 2, arg: step << 24 | 0x3002 };
            s.radio_starts.observe(&last);
            s.fault_counts = vec![FaultCount { code: last.code, site: last.site, count: 1, last }];
        });
        let failure = format!("Wi-Fi start failed ({name} returned error 0x3002)");
        assert_eq!(state_text(&state).faults, [format!("radio stopped: {failure}; it restarts by itself"),
            format!("{failure} at site 2, last at cycle 20; restarting")]);
    }
    let mut state = device(|s| {
        s.fault_counts.clear();
        Arc::make_mut(s.debug.as_mut().unwrap()).runtime = vec![(0x1000, "wifi_crypto_fault_aes_wrap")];
        s.radio_illegal_pc = Some(0x1006);
    });
    assert_eq!(state_text(&state).faults, ["Wi-Fi crypto aes_wrap is not available yet"]);
    state.radio_illegal_pc = Some(0x1007);
    assert!(state_text(&state).faults.is_empty());
}

#[test]
fn the_state_panel_names_posted_station_events() {
    for (arg, text) in [(2, "station started"), (3, "station stopped"), (4, "connected to the access point"),
        (5, "disconnected from the access point"), (1, "scan done"), (12, "Wi-Fi event 12"),
        (-2, "Wi-Fi event 4294967294"), (-1, "event from another source")] {
        let state = device(|s| {
            let last = FwFault { cycle: 20, code: watch::FAULT_RADIO_EVENT, site: watch::SITE_RADIO_SCHEDULER, arg };
            s.radio_starts.observe(&last);
            s.fault_counts = vec![FaultCount { code: last.code, site: last.site, count: 1, last }];
        });
        assert_eq!(state_text(&state).faults, [format!("{text} in the radio scheduler, last at cycle 20")]);
    }
}

#[test]
fn the_state_panel_keeps_the_fault_tally_across_radio_starts() {
    let mut s = device(|s| s.fault_counts.clear());
    let marker = |cycle, arg| FwFault { cycle, code: watch::FAULT_RADIO_STARTED, site: watch::SITE_RADIO_SCHEDULER, arg };
    s.radio_starts.observe(&marker(10, 1));
    assert!(state_text(&s).faults.is_empty(), "boot alone shows neither a restart nor a fault");

    let last = FwFault { cycle: 20, code: watch::FAULT_KERNEL_TIMER_LOST, site: 2, arg: 0x3fc9_1a20 };
    s.radio_starts.observe(&last);
    s.fault_counts.push(FaultCount { code: last.code, site: last.site, count: 1, last });
    let lost = "radio timer 0x3fc91a20 lost (no radio memory when armed; it won't fire) at site 2, last at cycle 20";
    assert_eq!(state_text(&s).faults, [
        "radio stopped: a radio timer was lost or overwritten; it restarts by itself", lost,
    ], "a lost timer degrades this start and enters the tally");

    for (cycle, arg, summary) in [
        (30, 0x0101_0002, "radio restarted once, last cause: request"),
        (40, 0x026b_0003, "radio restarted 2 times, last cause: timer lost, 2 in a row"),
        (50, 0x036e_0004, "radio restarted 3 times, last cause: timer overwritten, 3 in a row"),
        (60, 0x017f_0005, "radio restarted 4 times, last cause: report 127"),
        (70, 0x0180_0006, "radio restarted 5 times, last cause: cause 128"),
        (80, 0xff01_0007u32 as i32, "radio restarted 6 times, last cause: request, 255 or more in a row"),
        (90, 0x0102_0008, "radio restarted 7 times, last cause: radio fault"),
        (100, 0x0164_0009, "radio restarted 8 times, last cause: PHY start failed"),
        (110, 0x0172_000a, "radio restarted 9 times, last cause: Wi-Fi start failed"),
    ] {
        s.radio_starts.observe(&marker(cycle, arg));
        assert_eq!(state_text(&s).faults, [summary, lost], "the new start clears degradation and preserves the run's tally");
    }
}

/// What the run did, in the source: each condition in the colour of its newest value (of an
/// inlined condition's copies, the one that branched most since the publication read before),
/// the lines of statements that never ran dimmed, and a boolean variable's value coloured too.
#[test]
fn conditions_show_their_newest_value_and_code_that_never_ran_is_dimmed() {
    const SOURCE: &str = "fn tick() {\n    if mode == 1 {\n        go()\n    }\n    if pressed && !was {\n        x = 1\n    }\n}\n";
    let span = |text: &str| {
        let start = SOURCE.find(text).expect("fixture text") as u32;
        (start, start + text.len() as u32)
    };
    let stmt = |text: &str, line: u32| FwStmt { addr: Some(0x40 + line), span: span(text), line, col: 5, owner: fab_elec_mcu_emu::Owner::Handler(0) };
    let cond = |pc: u32, text: &str| FwCond { pc, span: span(text), line: 0, owner: fab_elec_mcu_emu::Owner::Handler(0), taken_true: true };
    let debug = Arc::new(FwDebug {
        handlers: vec![FwHandler { name: "tick".into(), span: (0, SOURCE.len() as u32) }],
        stmts: vec![
            stmt("if mode == 1 {\n        go()\n    }", 2),
            stmt("go()", 3),
            stmt("if pressed && !was {\n        x = 1\n    }", 5),
            stmt("x = 1", 6),
            // emitted no code: neither ran nor dimmed
            FwStmt { addr: None, ..stmt("x = 1", 6) },
        ],
        // `mode == 1` twice: a helper's condition inlined at two calls
        conds: vec![cond(0x50, "mode == 1"), cond(0x60, "mode == 1"), cond(0x70, "pressed && !was")],
        vars: vec![FwVar { name: "count".into(), ty: FwTy::I32, addr: 0x100, span: (0, 0) }, FwVar { name: "on".into(), ty: FwTy::Bool, addr: 0x104, span: (0, 0) }],
        ..FwDebug::default()
    });
    let count = |cond: u32, trues: u64, falses: u64, last: bool| FwCondCount { cond, trues, falses, last };
    let state = |counts: Vec<FwCondCount>| device(|s| {
        s.debug = Some(debug.clone());
        s.ran_counts = vec![(0, 9), (2, 9), (3, 2)];
        s.cond_counts = counts;
        s.vars = vec![FwValue::I32(3), FwValue::Bool(true)];
    });
    let lines = SourceLines::new(SOURCE);
    assert_eq!((lines.at(0), lines.at(SOURCE.find("go").unwrap())), ((0, 0), (2, 8)), "a byte's line and its offset in it");
    let read = state(vec![count(0, 0, 4, false), count(1, 9, 0, true)]);
    let newest = state(vec![count(0, 0, 5, false), count(1, 9, 0, true), count(2, 3, 2, true)]);
    let mut marks = Vec::new();
    cond_marks(&newest, Some(&read), &lines, &mut marks);
    let shown = |marks: &[CondMark]| marks.iter().map(|m| (m.line, &SOURCE.lines().nth(m.line).unwrap()[m.bytes.clone()], m.value)).collect::<Vec<_>>();
    assert_eq!(shown(&marks), [(1, "mode == 1", false), (4, "pressed && !was", true)],
        "each condition once, in its newest value: of the copies, the one that branched since the publication read before");
    cond_marks(&newest, None, &lines, &mut marks);
    assert_eq!(shown(&marks)[0], (1, "mode == 1", true), "with no publication read before, the copy that branched most");
    let mut dims = Vec::new();
    unrun_lines(&newest, &lines, &mut dims);
    assert_eq!(dims, [2..3], "the line of the statement that never ran, and no other: the `if` before it ran, its closing brace is the `if`'s");
    let text = state_text(&newest);
    assert_eq!(rows(&text), [("count", "3", None), ("on", "true", Some(true)), ("PC", "0x0052", None), ("cycle", "20000", None)],
        "each variable keeps its value, and only the boolean takes the condition colour");
}

fn widgets(vm: &mut makepad_widgets_core::ScriptVm) {
    makepad_widgets_core::script_mod_with(vm, Default::default(), &[makepad_widgets_pickers::pickers_mod, makepad_widgets_rich_text::rich_text_mod]);
    makepad_code_editor::script_mod(vm);
}

/// The pane's layout, its state panel included, names only properties its widgets declare: it
/// registers and builds without a script error.
#[test]
fn the_firmware_pane_registers_and_builds_without_a_script_error() {
    makepad_widgets_core::test_cx::on_test_cx(|| {
        let mut cx = makepad_widgets_core::test_cx::checkout_test_cx(widgets);
        cx.with_vm(|vm| {
            let _ = vm.take_errors();
            vm.capture_errors();
            fab_elec_view::live::fw_steps::script_mod(vm);
            fab_elec_view::fw_rows::script_mod(vm);
            fab_elec_view::firmware::script_mod(vm);
            let _pane = <fab_elec_view::firmware::FirmwarePane as makepad_widgets_core::makepad_script::ScriptNew>::script_new_with_default(vm);
            let errors = vm.take_errors();
            assert!(errors.is_empty(), "{errors:#?}");
        });
    });
}

/// The source shows the device two ways that never mix (Q2): paused or stopped, one marker on the
/// statement about to execute (hollow on the last that ran while asleep); playing, a heat on the
/// lines each publication ran that fades in wall time. Both name lines of the image's own text only.
#[test]
fn the_source_shows_one_marker_while_paused_and_a_fading_heat_while_playing() {
    assert_eq!(marker(&device(|_| {})), Some(Mark::Solid(6)), "a step's stop marks the statement about to execute (line 7)");
    assert_eq!(marker(&device(|s| s.pc = 0x60)), Some(Mark::Hollow(1)),
        "a stop where no statement begins (a handler's exit) marks the last that ran, hollow");
    let paused = |at: At| device(move |s| {
        s.stopped = None;
        s.at = at;
        s.last = Some(1);
    });
    assert_eq!(marker(&paused(At::Statement(1))), Some(Mark::Solid(6)), "paused mid-run: the statement executing");
    assert_eq!(marker(&paused(At::Runtime("delay"))), Some(Mark::Solid(6)), "in the runtime on a statement's behalf: that statement");
    assert_eq!(marker(&paused(At::Sleeping)), Some(Mark::Hollow(6)), "asleep: the last statement that ran, hollow");
    assert_eq!(marker(&paused(At::Reset)), None, "before any statement: no marker");
    assert_eq!(marker(&device(|s| s.debug = None)), None, "without a table nothing places the device");

    let mut lines = Vec::new();
    ran_lines(&device(|s| s.ran = vec![(1, 3), (0, 1), (1, 1)]), None, &mut lines);
    assert_eq!(lines, [1, 6], "the lines a publication ran, ascending, each once");

    let mut heat = Heat::default();
    heat.pulse(&[1, 6], 10.0);
    assert_eq!(heat.washes(10.0), [(1..2, HEAT_PEAK), (6..7, HEAT_PEAK)], "a line that ran is at full heat");
    let half = 10.0 + HEAT_FADE / 2.0;
    heat.pulse(&[6], half);
    let washes = heat.washes(half);
    assert!(washes[0].0 == (1..2) && (washes[0].1 - HEAT_PEAK / 2.0).abs() < 1e-4, "heat fades linearly in wall time: {washes:?}");
    assert_eq!(washes[1], (6..7, HEAT_PEAK), "a line that runs again pulses back to full heat");
    let washes = heat.washes(10.0 + HEAT_FADE * 1.01);
    assert!(washes.len() == 1 && washes[0].0 == (6..7), "a line gone cold drops out: {washes:?}");
    assert!(heat.washes(half + HEAT_FADE * 1.01).is_empty() && !heat.is_hot(), "all cold: no washes, no frames asked for");

    // The editor names lines of the image's text only while it shows exactly that text.
    let mut shown = FirmwareShown::default();
    let benches = Arc::new(vec![bench(UnitState::Built, "ledavr.fw.splash")]);
    assert!(!shown.shows_rev(3), "nothing loaded: no revision shown");
    shown.update(None, &benches, Some(&docs(3, "loop {}")), None).expect("the document loads");
    assert!(shown.shows_rev(3) && !shown.shows_rev(4), "the loaded revision, and no other");
    assert!(shown.typed(0.1, || "loop { a }".into()).is_some(), "typing goes to the engine");
    assert!(!shown.shows_rev(3), "typing on the way: the editor no longer shows revision 3's text");
    shown.update(None, &benches, Some(&docs(4, "loop { a }")), None);
    assert!(shown.shows_rev(4), "once the edit applied, the editor shows revision 4 exactly");
}

/// The heat of a statement that ran only in a publication the pane never read (the newest replaced
/// it) still pulses: the pane compares the device's cumulative counts with those it read before.
#[test]
fn a_statement_in_a_publication_dropped_unread_still_pulses() {
    // read: statement 0 (line 2) has run four times
    let read = device(|s| s.ran_counts = vec![(0, 4)]);
    // dropped unread: statement 1 (line 7) ran once; the newest then names only statement 0 again
    let _dropped = device(|s| {
        s.ran = vec![(1, 1)];
        s.ran_counts = vec![(0, 4), (1, 1)];
    });
    let newest = device(|s| {
        s.ran = vec![(0, 1)];
        s.ran_counts = vec![(0, 5), (1, 1)];
    });
    let mut lines = Vec::new();
    ran_lines(&newest, Some(&read), &mut lines);
    assert_eq!(lines, [1, 6], "statement 1, which ran only in the dropped publication, is among the lines that ran");
    let mut heat = Heat::default();
    heat.pulse(&lines, 20.0);
    assert_eq!(heat.washes(20.0), [(1..2, HEAT_PEAK), (6..7, HEAT_PEAK)], "and its line pulses to full heat");

    ran_lines(&newest, Some(&newest), &mut lines);
    assert!(lines.is_empty(), "nothing grew since: nothing pulses, even though the publication names statement 0");
    let others = [
        ("another image", device(|s| s.source_rev = 2)),
        ("another device (a count went down)", device(|s| s.ran_counts = vec![(0, 9)])),
        ("another device (a statement counted before is gone)", device(|s| s.ran_counts = vec![(0, 1), (1, 1), (2, 1)])),
    ];
    for (what, seen) in others {
        ran_lines(&newest, Some(&seen), &mut lines);
        assert_eq!(lines, [1], "{what}: the counts do not compare, so the statements the publication names");
    }
}

/// The firmware's steps, the transport's and the state panel's: shown while a firmware is bound;
/// each step only where the device takes it, by the device's own rules on its published state.
#[test]
fn the_firmware_steps_offer_what_the_device_can_take() {
    // the fixture's statements begin at 0x40 and 0x52; setup's entry is 0x3c, tick's 0x50; a call
    // into a handler returns to 0x70
    let map = StepMap { funcs: Vec::new(), calls: Vec::new(), owners: Vec::new(), statements: vec![0x40, 0x52], entries: vec![(0x3c, 0), (0x50, 1)], returns: vec![0x70] };
    let state = |pc: u32, handler: Option<u32>, map: Option<StepMap>, halted: bool| {
        let mut s = device(|s| {
            s.pc = pc;
            s.handler = handler;
            if halted {
                s.at = At::Halted(Halt { tick: 19_990, pc, kind: HaltKind::Break, what: "break" });
            }
        });
        let mut debug = (**s.debug.as_ref().expect("the fixture has a table")).clone();
        debug.map = map;
        s.debug = Some(Arc::new(debug));
        s
    };
    let co = |pc, handler, map, halted| CoState(Arc::new(state(pc, handler, map, halted)));
    let at = |pc: u32, handler: Option<u32>| steps(true, Some(&co(pc, handler, Some(map.clone()), false)));
    let (none, every) = (Steps { shown: true, step: false, out: false }, Steps { shown: true, step: true, out: true });
    let no_out = Steps { out: false, ..every };
    assert_eq!(steps(false, None), Steps::default(), "no firmware bound: no steps");
    assert_eq!(steps(false, Some(&co(0x52, Some(1), Some(map.clone()), false))), Steps::default(),
        "a device's state without a bound firmware offers nothing");
    assert_eq!(steps(true, None), none, "bound, before its device publishes: shown, none can go");
    assert_eq!(steps(true, Some(&CoState(Arc::new(7u32)))), none, "a state that is not a firmware's offers no step");
    assert_eq!(steps(true, Some(&co(0x52, Some(1), None, false))), none, "an image without a step map takes no step");
    assert_eq!(steps(true, Some(&co(0x52, Some(1), Some(map.clone()), true))), none, "a halted device takes no step");
    let asleep = at(0x80, None);
    assert_eq!(asleep, no_out, "no handler running: the statement and handler steps, no step out");
    assert!(asleep.offers(StepTo::Statement) && asleep.offers(StepTo::HandlerEntry) && !asleep.offers(StepTo::HandlerExit),
        "a press goes only for the steps offered");
    assert_eq!(at(0x52, Some(1)), every, "inside a handler: every step");
    assert_eq!(at(0x50, None), every, "stopped on a handler's entry (a Handler step's stop): it has been called, so it can step out");
    let entry = state(0x50, None, Some(map.clone()), false);
    assert_eq!((state_text(&entry).at.as_str(), marker(&entry)), ("Stopped before tick, line 7", Some(Mark::Solid(6))),
        "on a handler's entry the panel and the marker name the handler entered and its first statement, not the last that ran");
    assert_eq!(at(0x70, Some(1)), no_out, "on a return: the handler has returned, no step out");

    // Beside the transport's strip the steps fold to icons where their words (222 points) would
    // leave the strip under 600 points; the row's width alone decides, so it never flips.
    let (words, wide) = (222.0, 600.0);
    assert!(!fold_to_icons(655.0 + words, words, wide), "a row with room for the strip beside the words keeps them");
    assert!(!fold_to_icons(wide + words, words, wide), "exactly the width keeps the words");
    assert!(fold_to_icons(wide + words - 1.0, words, wide), "a point less folds them to icons");
    assert!(fold_to_icons(271.0 + words, words, wide), "the 1280x800 row's strip (271 points beside the words) gets the icons");
}

/// The state panel stays readable in a short pane: idle pins (inputs in their reset state that
/// nothing drives) fold into one line that names each, and the panel takes two columns when it is
/// shorter than its content and the pane is wide enough for both beside the source.
#[test]
fn idle_pins_fold_and_a_short_panel_takes_two_columns() {
    let idle = |pin| FwPin { pin, mode: PinUse::Input, level: None, duty: 0.0 };
    let s = device(|s| {
        s.pins.insert(1, idle("PB2"));
        s.pins.extend([idle("PC0"), idle("PC1"), FwPin { pin: "PD3", mode: PinUse::Input, level: Some(false), duty: 0.0 }]);
    });
    let text = state_text(&s);
    assert_eq!(text.pins, ["PB1  PWM 25 %, high", "PB0  out, high", "PD2  in, pull-up, ?", "PD3  in, low"],
        "the pins in use, in table order (an input with a level is in use)");
    assert_eq!(text.pin_names, ["PB1", "PB0", "PD2", "PD3"], "each line keeps its pin for the hover");
    assert_eq!(text.idle_pins, "idle  PB2 PC0 PC1", "the idle pins, each by name");
    assert_eq!(state_text(&device(|_| {})).idle_pins, "", "no idle pins: no line");

    let wide = SOURCE_MIN + STATE_PANEL_TWO;
    assert!(state_columns(false, wide, 300.0, 400.0), "a short panel in a wide pane takes two columns");
    assert!(!state_columns(false, wide - 1.0, 300.0, 400.0), "a narrower pane keeps one column, which scrolls");
    assert!(!state_columns(false, wide, 300.0, 280.0), "content that fits keeps one column");
    assert!(state_columns(true, wide, 300.0, 290.0), "two columns stay until the content fits with room to spare");
    assert!(!state_columns(true, wide, 300.0, 270.0), "and then go back to one");
    assert!(!state_columns(false, wide, 0.0, 400.0) && state_columns(true, wide, 0.0, 400.0),
        "a panel that was not drawn (folded away) keeps its layout");
    assert!(!state_columns(true, wide - 1.0, 0.0, 400.0), "but a narrower pane has one column");
}

/// The state panel's pins, where a hover lights a pin's net in every view: each line names its pin,
/// and the pointer's height picks the line.
#[test]
fn a_pointer_over_the_pins_names_the_pin_of_its_line() {
    let s = device(|_| {});
    let text = state_text(&s);
    assert!(text.pins.len() >= 2, "the fixture shows several pins");
    assert_eq!(text.pin_names, s.pins.iter().map(|p| p.pin).collect::<Vec<_>>(), "one name per line, in the lines' order");
    assert_eq!(text.pin_names.len(), text.pins.len(), "a name for each line");
    // three lines 12 points high from y = 100
    assert_eq!(line_at(100.0, 36.0, 3, 100.0), Some(0), "the first line's top");
    assert_eq!(line_at(100.0, 36.0, 3, 111.9), Some(0), "the first line's bottom");
    assert_eq!(line_at(100.0, 36.0, 3, 112.0), Some(1), "the second line starts a line height down");
    assert_eq!(line_at(100.0, 36.0, 3, 135.9), Some(2), "the last line's bottom");
    for (y, why) in [(99.9, "above"), (136.0, "below"), (f64::NAN, "nowhere")] {
        assert_eq!(line_at(100.0, 36.0, 3, y), None, "{why} the lines: no pin");
    }
    assert_eq!(line_at(100.0, 0.0, 3, 100.0), None, "lines with no height: no pin");
    assert_eq!(line_at(100.0, 36.0, 0, 110.0), None, "no pins: no pin");
}

#[test]
fn state_and_source_marker_name_a_shared_function_owner() {
    let mut s = device(|s| {
        let d = Arc::make_mut(s.debug.as_mut().unwrap());
        d.funcs.push(fab_elec_mcu_emu::FwFunc { name: "helper".into(), span: (20, 31) });
        d.stmts[1].owner = fab_elec_mcu_emu::Owner::Func(0);
        d.map = Some(StepMap { funcs: vec![(0x50, 0x60, 0)], ..StepMap::default() });
        s.chain = vec![0];
    });
    assert_eq!(state_text(&s).at, "Stopped before helper, line 7");
    s.pc = 0x50;
    assert_eq!(marker(&s), Some(Mark::Solid(6)), "function prologue points to its first statement");
    assert_eq!(state_text(&s).at, "Stopped before helper, line 7");
    s.at = At::Statement(1);
    s.stopped = None;
    assert_eq!(state_text(&s).at, "helper, line 7", "running helper is named independently of its caller");
}

/// What an outside edit of a firmware document changed, for its flash (V9.3): the lines between
/// the texts' common head and tail; a deletion marks the line after it; equal lines nothing.
#[test]
fn an_outside_edit_names_the_firmware_lines_it_changed() {
    use fab_elec_view::live::views::changed_lines;
    let before = "a\nb\nc\nd\n";
    assert_eq!(changed_lines(before, before), None, "equal texts change nothing");
    assert_eq!(changed_lines(before, "a\nB\nc\nd\n"), Some(1..2), "one line changed");
    assert_eq!(changed_lines(before, "a\nb\nx\ny\nc\nd\n"), Some(2..4), "two lines inserted");
    assert_eq!(changed_lines(before, "a\nd\n"), Some(1..2), "a deletion marks the line after it");
    assert_eq!(changed_lines(before, "a\nb\nc\nd\ne\n"), Some(4..5), "a line added at the end");
}

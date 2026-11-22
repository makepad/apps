#[allow(dead_code)]
#[path = "support/island_fixture.rs"]
mod island_fixture;
#[path = "../../s3/tests/support/mod.rs"]
mod s3_fixture;

use std::collections::BTreeSet;
use std::sync::{Arc, OnceLock};

use fab_elec_mcu::{compile, Diagnostic};
use fab_elec_mcu_emit_s3::{build_image, build_slot, chip, firmware, flash_regions, island_regions, image::{RestartPolicy, StartTask}, radio,
    Board, BoardTime, BootTask, Firmware, Image, Radio, RadioSram, SlotOptions, Wiring, SRAM_START, STACK_TOP, VOICEBOX};
use fab_elec_mcu_emu::{bus::Width, HaltKind, Machine, PcSet, Ran, Status, Until};
use radio::phy_glue::PHY_PACK_SYMBOLS;
use radio::stand_in::{self as si, row_names, stand_in, ENV_WORDS, E_CONFIG, STATE_OBJECT};
use fab_elec_mcu_island::{import_map::parse_roots, Archive, Island};
use fab_elec_mcu_rt::{module, watch};
use fab_elec_mcu_s3::{rom, Ctx, ForeignBus, ForeignCode, Next, S3Machine, S3Profile, VirtualFn};

#[test]
fn rows_cover_the_station_contract_exactly_once() {
    let roots = parse_roots(include_str!("../../island/packs/esp32s3-idf-v6.1.1-sta.roots")).unwrap();
    let expected: BTreeSet<_> = roots.iter().map(String::as_str).chain(PHY_PACK_SYMBOLS)
        .chain(["ppTask", "wDev_ProcessFiq", "pp_timer_process"]).collect();
    let mut names = BTreeSet::new();
    for name in row_names() { assert!(names.insert(name), "duplicate stand-in row {name}"); }
    assert_eq!(names, expected);
}

fn linked(names: &[&str]) -> Island { linked_with(names, vec![0; 4]) }

fn linked_with(names: &[&str], initial_data: Vec<u8>) -> Island {
    let (_, archive) = island_fixture::station_archive_with_virtual_roots(initial_data, 4, names);
    radio::link_pack(&[Archive { name: "station.a", data: &archive }]).unwrap()
}

fn station_firmware(island: Option<&Island>, profile: &S3Profile) -> Result<Firmware, Vec<Diagnostic>> {
    let wiring = Wiring { i2c: None, motion: None, panel: None, touch: None, audio: None };
    let time = BoardTime { rc_slow_hz: profile.time.rc_slow_hz, wdt_delay_sel: profile.time.wdt_delay_sel };
    firmware("fn setup() {}", &[], &wiring, time, island, &[])
}

fn profile(island: &Island, start: Option<StartTask>) -> Radio {
    Radio { island: island.binding(), start, restart: RestartPolicy::PROVISIONAL,
        dispatcher_priority: 1, irq_stack_bytes: 2048, task_capacity: 3, isr_capacity: radio::PACK_ISR_LINES,
        heap_bytes: 16 * 1024, timer_reserve: 8, trace_entries: 0, burst_insns: 0, boot_tasks: vec![] }
}

fn fixture(names: &[&str], start: Option<StartTask>) -> (Island, RadioSram) {
    let island = linked(names);
    let radio = profile(&island, start);
    let app = compile("fn setup() {}", &chip::profile(), &[], &[]).unwrap();
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let slot = build_slot(&module, &SlotOptions { radio: Some(radio), ..SlotOptions::default() }).unwrap();
    (island, slot.layout.radio.unwrap())
}

#[test]
fn table_binds_each_fixture_function_and_the_image_addresses() {
    let (island, radio) = fixture(&row_names().collect::<Vec<_>>(), Some(StartTask { priority: 3, stack_bytes: 4096 }));
    let mut binding = island.binding();
    let parts = stand_in(&binding, &radio).unwrap();
    assert_eq!(parts.ranges, island.code.iter().map(|r| r.start..r.start + r.bytes).collect::<Vec<_>>());
    assert_eq!(parts.functions.len(), row_names().count());
    assert_eq!(parts.env.len(), ENV_WORDS);
    assert_eq!(parts.env, [
        radio.osi_table, island.glue[STATE_OBJECT], island.functions["ppTask"].start,
        island.functions["wDev_ProcessFiq"].start, island.functions["pp_timer_process"].start,
        island.glue["wifi_event_base_name"], radio.phy_cal_data, radio.wifi_config.unwrap(), radio.phy_dig_regs,
        island.objects[radio::PHY_FUNS_OBJECT].start,
    ]);
    for f in &parts.functions {
        assert_eq!((f.address, f.bytes), (island.functions[&f.name].start, 16), "{}", f.name);
    }
    let table = parts.table().unwrap();
    for (_, address) in &binding.entries { assert!(table.contains(*address)); }
    assert!(!table.contains(island.glue["radio_start"]));

    // A short or absent unimplemented symbol still leaves its pack PCs foreign.
    let missing = binding.functions.remove("esp_wifi_stop").unwrap();
    let short = binding.functions.get_mut("esp_wifi_scan_stop").unwrap();
    short.bytes = 14;
    let short_address = short.start;
    let mut without_config = radio;
    without_config.wifi_config = None;
    let parts = stand_in(&binding, &without_config).unwrap();
    assert_eq!(parts.env[E_CONFIG], 0);
    assert!(!parts.functions.iter().any(|f| f.address == missing.start || f.address == short_address));
    let table = parts.table().unwrap();
    assert!(table.contains(missing.start) && table.contains(short_address));

    binding.objects.remove(radio::PHY_FUNS_OBJECT).unwrap();
    let error = stand_in(&binding, &without_config).err().expect("g_phyFuns is required");
    assert!(error.contains(radio::PHY_FUNS_OBJECT), "{error}");
}

#[test]
fn missing_pp_task_names_the_required_callback() {
    let names: Vec<_> = row_names().filter(|&name| name != "ppTask").collect();
    let (island, radio) = fixture(&names, None);
    assert!(!island.functions.contains_key("ppTask"));
    // Both the env and the task row need the linked callback.
    let error = stand_in(&island.binding(), &radio).err().expect("ppTask is required");
    assert!(error.contains("ppTask"), "{error}");
    let errors = station_firmware(Some(&island), &s3_fixture::profile(53)).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("stand-in") && d.message.contains("ppTask")), "{errors:?}");
}

#[test]
fn firmware_reports_a_pack_function_overlapping_a_rom_entry() {
    let mut island = linked(&row_names().collect::<Vec<_>>());
    island.functions.get_mut("esp_wifi_stop").unwrap().start = rom::I2C_READ_REG;
    let errors = station_firmware(Some(&island), &s3_fixture::profile(53)).unwrap_err();
    assert!(errors.iter().any(|d| d.message.contains("foreign table") && d.message.contains("overlaps")), "{errors:?}");
}

// Scratch SRAM between the app allocations and stack. The virtual boot caller has
// its own fetch range. Its CALLX8 targets come from words seeded before handoff.
const CALLER_DATA: u32 = STACK_TOP - 0x4000;
const CALLER: u32 = 0x4037_8000 + (CALLER_DATA - SRAM_START);
const TARGET: u32 = CALLER_DATA + 32;
const ARGS: u32 = TARGET + 4;
const DELAY_SLOT: u32 = ARGS + 12;
const FREE_SLOT: u32 = DELAY_SLOT + 4;
const MAGIC_PTR: u32 = FREE_SLOT + 4;
const RESULT: u32 = MAGIC_PTR + 4;
const SECOND_RESULT: u32 = RESULT + 4;
const MODE_OUT: u32 = SECOND_RESULT + 4;
const SMALL_HEAP: u32 = 4608;
type Body = fn(&Ctx, &dyn ForeignBus) -> Next;

struct Station { island: Island, image: Image }

impl Station {
    fn new(heap_bytes: u32, caller_priority: u32) -> Self {
        let island = linked(&row_names().collect::<Vec<_>>());
        let start = StartTask { priority: 5, stack_bytes: 4096 };
        let caller = BootTask { entry: CALLER, abi: radio::TaskAbi::Windowed, priority: caller_priority,
            stack_bytes: 2048, args: [0; 6] };
        let radio = Radio { heap_bytes, task_capacity: 5, boot_tasks: vec![caller], ..profile(&island, Some(start)) };
        let board = Board { radio: Some(radio), layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None,
            time: BoardTime { rc_slow_hz: (136_000, 1), wdt_delay_sel: 0 } };
        let app = compile("fn setup() {}", &chip::profile(), &[], &[]).unwrap();
        let module = module::load(&app.module, &chip::profile()).unwrap();
        let image = build_image(&module, &board, &SlotOptions::default()).unwrap();
        assert!(image.slot.layout.regions[0].used <= u64::from(CALLER_DATA));
        assert!(MODE_OUT + 4 <= STACK_TOP - image.slot.layout.stack_bytes);
        Self { island, image }
    }

    fn radio(&self) -> &RadioSram { self.image.slot.layout.radio.as_ref().unwrap() }
    fn entry(&self, name: &str) -> u32 { self.island.functions[name].start }
    fn field(&self, m: &S3Machine, index: u32) -> u32 { word(m, self.island.glue[STATE_OBJECT] + 4 * index) }
    fn gate(&self, name: &str) -> u32 {
        self.image.slot.symbols.radio.as_ref().unwrap().gate_reports.iter().find(|g| g.name == name).unwrap().address
    }
    fn machine(&self, body: Body, target: u32, args: [u32; 3]) -> S3Machine {
        let r = self.radio();
        let mut parts = stand_in(&self.island.binding(), r).unwrap();
        parts.ranges.push(CALLER..CALLER + 16);
        parts.functions.push(VirtualFn { name: "test caller".into(), address: CALLER, bytes: 16, frame: 144, body });
        let mut m = island_fixture::machine_with_island(&self.image.flash, &self.island.image, &s3_fixture::profile(53))
            .with_foreign(Arc::new(parts.table().unwrap()));
        let slot = |name: &str| r.osi_table + 4 * radio::adapter::OSI_FIELDS.iter().position(|&(field, _)| field == name).unwrap() as u32;
        let words = [target, args[0], args[1], args[2], slot("_task_delay"), slot("_get_free_heap_size"),
            r.wifi_config.unwrap() + 156, u32::MAX, u32::MAX, u32::MAX];
        let bytes: Vec<_> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        assert!(m.seed_data(TARGET, &bytes));
        m
    }
}

fn station() -> &'static Station {
    static STATION: OnceLock<Station> = OnceLock::new();
    // Start (5) blocks on ppTask (kernel priority 3). The test caller (2) runs after both park.
    STATION.get_or_init(|| Station::new(16 * 1024, 2))
}

fn before_init() -> &'static Station {
    static STATION: OnceLock<Station> = OnceLock::new();
    // The probe runs immediately before the start task, using a separate boot stack.
    STATION.get_or_init(|| Station::new(SMALL_HEAP, 6))
}

fn word(m: &S3Machine, p: u32) -> u32 {
    let mut bytes = [0; 4];
    assert!(m.read_data(p, &mut bytes), "SRAM word at {p:#x}");
    u32::from_le_bytes(bytes)
}

fn reach(m: &mut S3Machine, pc: u32) {
    let mut marks = PcSet::new(pc, 1);
    marks.insert(pc);
    assert_eq!(m.run(&Until { tick: m.tick_hz(), marks: &marks, stops: &[pc], stops_after: m.now(), on_drive: false }),
        Ran::Pc, "reach {pc:#x}: {:?} {:?}", m.status(), m.explain_foreign());
}

fn run_until(m: &mut S3Machine, tick: u64) -> Ran {
    m.run(&Until { tick, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false })
}

fn reports(m: &mut S3Machine) -> Vec<(u8, i32)> {
    let mut out = Vec::new();
    m.take_out(&mut out);
    let mut reader = watch::Reader::new();
    let mut reports = Vec::new();
    let mut seq = 0u8;
    for byte in out {
        assert_eq!(byte.link, 0);
        if let Some(frame) = reader.push(byte.byte) {
            let frame = frame.unwrap();
            assert_eq!(frame.seq, seq);
            seq = seq.wrapping_add(1);
            match frame.event {
                watch::Event::Fault { code, arg, .. } => reports.push((code, arg)),
                watch::Event::Lost { .. } => panic!("lost station report"),
                _ => {}
            }
        }
    }
    reports
}

#[test]
fn voicebox_firmware_starts_the_station_with_its_own_flash_and_table() {
    let profile = s3_fixture::profile(53);
    let island = linked(&row_names().collect::<Vec<_>>());
    let plain = station_firmware(None, &profile).unwrap();
    assert!(plain.radio.is_none());
    let built = station_firmware(Some(&island), &profile).unwrap();
    let radio = built.radio.as_ref().unwrap();
    let table = radio.table();
    assert!(table.contains(rom::I2C_READ_REG));
    assert!(built.flash.len() <= VOICEBOX.slots[1] as usize);
    let flash = radio.flash(&built.flash);
    let at = VOICEBOX.island as usize;
    assert!(flash[..built.flash.len()] == built.flash[..], "machine flash preserves the board image");
    assert!(flash[built.flash.len()..at].iter().all(|&byte| byte == 0xff));
    assert!(flash[at..] == island.image[..], "the linked island begins at its reserved offset");
    // Flash to writes the head, slot A and the island: each write is the simulator's flash there.
    for (offset, bytes) in flash_regions(&built.flash).into_iter().chain(island_regions(&radio.island).unwrap()) {
        let offset = offset as usize;
        assert!(flash[offset..offset + bytes.len()] == bytes[..], "the board write at 0x{offset:x} differs from the simulator's flash");
    }
    assert_eq!(island_regions(&radio.island).unwrap()[0].0 as usize + island_regions(&radio.island).unwrap()[0].1.len(), flash.len(),
        "the island's body write ends the flash");

    let mut machine = S3Machine::new(Arc::new(flash), &profile).with_foreign(table);
    let &(drain, _) = built.debug.runtime.iter().find(|(_, name)| *name == "radio_events").unwrap();
    reach(&mut machine, drain);
    let end = machine.now() + machine.tick_hz() / 100;
    assert_eq!(run_until(&mut machine, end), Ran::Tick, "{:?}", machine.explain_foreign());
    assert_eq!(reports(&mut machine), [(watch::FAULT_RADIO_STARTED, 1), (watch::FAULT_RADIO_EVENT, 2)]);
}

/// Flash to's island writes as the ROM loader performs them (`esp_rom::esp_write`): each region's
/// FLASH_BEGIN erases its whole sectors, then FLASH_DATA programs 1024-byte packets in order
/// (programming only clears bits). Each step is one erase or one packet.
fn rom_steps(regions: &[(u32, &[u8])]) -> Vec<(usize, Option<usize>)> {
    let mut steps = Vec::new();
    for (index, (_, bytes)) in regions.iter().enumerate() {
        steps.push((index, None));
        steps.extend((0..bytes.len().div_ceil(1024)).map(|packet| (index, Some(packet))));
    }
    steps
}

fn rom_apply(flash: &mut [u8], regions: &[(u32, &[u8])], (index, packet): (usize, Option<usize>)) {
    let (offset, bytes) = (regions[index].0 as usize, regions[index].1);
    match packet {
        None => flash[offset..offset + bytes.len().next_multiple_of(4096)].fill(0xff),
        Some(packet) => {
            let chunk = &bytes[packet * 1024..((packet + 1) * 1024).min(bytes.len())];
            for (cell, byte) in flash[offset + packet * 1024..].iter_mut().zip(chunk) { *cell &= byte; }
        }
    }
}

#[test]
fn a_cut_island_write_never_starts_a_half_written_island() {
    let profile = s3_fixture::profile(53);
    let names = row_names().collect::<Vec<_>>();
    let island = linked(&names);
    let older = linked_with(&names, vec![1; 4]);
    assert_ne!(older.header, island.header, "the board's older island has another header");
    let built = station_firmware(Some(&island), &profile).unwrap();
    let radio = built.radio.as_ref().unwrap();
    let at = VOICEBOX.island as usize;
    let regions = island_regions(&radio.island).unwrap();
    let body = island.header.body_offset() as usize;
    assert_eq!(regions, [(VOICEBOX.island + body as u32, &radio.island[body..]), (VOICEBOX.island, &radio.island[..body])],
        "the body is written first and the header sectors last");
    let steps = rom_steps(&regions);
    // The island commits with the header sectors' first packet; the rest of those sectors is erased padding.
    let commit = steps.iter().position(|&step| step == (1, Some(0))).unwrap() + 1;
    assert!(radio.island[1024.min(body)..body].iter().all(|&b| b == 0xff), "past the first packet the header sectors are padding");
    let span = radio.island.len().max(older.image.len()).next_multiple_of(4096);
    // The cut comes after the head and slot A, written first: the board holds this image's slot.
    for (prior, before) in [("erased", None), ("an older island", Some(&older.image))] {
        let mut board = radio.flash(&built.flash)[..at].to_vec();
        board.resize(at + span, 0xff);
        if let Some(old) = before { board[at..at + old.len()].copy_from_slice(old); }
        let mut seen = std::collections::HashSet::new();
        for cut in 0..=steps.len() {
            let mut flash = board.clone();
            for &step in &steps[..cut] { rom_apply(&mut flash, &regions, step); }
            if !seen.insert(flash[at..].to_vec()) { continue; }
            let whole = flash[at..at + radio.island.len()] == radio.island[..];
            let mut machine = S3Machine::new(Arc::new(flash), &profile).with_foreign(radio.table());
            // Run in 5 ms steps until the radio's outcome is reported (the boot reports it once).
            let (mut reader, mut out, mut codes) = (watch::Reader::new(), Vec::new(), Vec::new());
            while !codes.iter().any(|&c| c == watch::FAULT_RADIO_STARTED || c == watch::FAULT_RADIO_ISLAND_MISMATCH) {
                let next = machine.now() + machine.tick_hz() / 200;
                assert!(next <= machine.tick_hz() / 5, "over {prior}, cut after {cut}: no radio outcome by 200 ms");
                run_until(&mut machine, next);
                machine.take_out(&mut out);
                for byte in out.drain(..) {
                    if let Some(watch::Event::Fault { code, .. }) = reader.push(byte.byte).map(|frame| frame.unwrap().event) { codes.push(code); }
                }
            }
            let started = codes.contains(&watch::FAULT_RADIO_STARTED);
            let refused = codes.contains(&watch::FAULT_RADIO_ISLAND_MISMATCH);
            assert_eq!((started, refused), (cut >= commit, cut < commit),
                "over {prior}, cut after {cut} of {} steps: the radio starts only once the header is written", steps.len());
            assert!(!started || whole, "over {prior}, cut after {cut}: a started island is the whole island");
        }
    }
}

fn call(target_at: u32, a: u32, b: u32, c: u32) -> Next {
    Next::Call { target_at, args: [a, b, c, 0, 0, 0] }
}

fn park(bus: &dyn ForeignBus) -> Next {
    match bus.sram_word(DELAY_SLOT) {
        Some(slot) => call(slot, u32::MAX, 0, 0),
        None => Next::Stop("missing test delay slot"),
    }
}

fn call_target(_: &Ctx, bus: &dyn ForeignBus) -> Next {
    match [0, 4, 8].map(|offset| bus.sram_word(ARGS + offset)) {
        [Some(a), Some(b), Some(c)] => call(TARGET, a, b, c),
        _ => Next::Stop("missing test arguments"),
    }
}

fn save_result<const AT: u32>(ctx: &Ctx, bus: &mut dyn ForeignBus) {
    if let Some(value) = ctx.result { let _ = bus.write(AT, Width::W, value, 0); }
}

fn get_mode_twice(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    match ctx.step {
        0 => call(TARGET, 0, 0, 0),
        1 => Next::Effect(save_result::<RESULT>),
        2 => call(TARGET, MODE_OUT, 0, 0),
        3 => Next::Effect(save_result::<SECOND_RESULT>),
        4 => park(bus),
        _ => Next::Stop("the parked test caller resumed"),
    }
}

#[test]
fn real_start_and_get_mode_agree_across_a_clone_of_blocked_init() {
    let s = station();
    let r = s.radio();
    let mut m = s.machine(get_mode_twice, s.entry("esp_wifi_get_mode"), [0; 3]);
    let start_task = r.boot_tasks[1];
    reach(&mut m, s.gate("osi_semphr_take"));
    assert_eq!(m.cpu().ar[2], s.field(&m, si::SEM));
    reach(&mut m, s.entry("ppTask"));
    assert_eq!(word(&m, start_task + radio::TCB_STATE), radio::BLOCKED);
    assert_eq!(word(&m, r.current), s.field(&m, si::TASK));
    let mut cloned = m.clone();

    let drain = s.image.slot.map.runtime.iter().find(|r| r.name == "radio_events").unwrap().start;
    for machine in [&mut m, &mut cloned] {
        // These PCs also establish the FIRST order independently of the checker's tags.
        for name in ["esp_wifi_set_mode", "esp_wifi_start", "phy_init_param_set", "phy_bbpll_en_usb",
            "register_chipv7_phy", "phy_wifi_enable_set"] {
            reach(machine, s.entry(name));
        }
        reach(machine, CALLER);
        assert_eq!(word(machine, start_task + radio::TCB_STATE), radio::BLOCKED);
        reach(machine, drain);
        let end = machine.now() + machine.tick_hz() / 100;
        assert_eq!(run_until(machine, end), Ran::Tick, "{:?}", machine.explain_foreign());
    }
    assert_eq!(m.same_state(&cloned), None, "includes all SRAM, the output stream and the CPU");
    let expected = [(watch::FAULT_RADIO_STARTED, 1), (watch::FAULT_RADIO_EVENT, 2)];
    assert_eq!(reports(&mut m), expected);
    assert_eq!(reports(&mut cloned), expected);
    assert!(!matches!(m.status(), Status::Halted(_)));
    assert_eq!(word(&m, s.island.objects[radio::PHY_FUNS_OBJECT].start), RadioSram::ROM_DATA_START);
    assert_eq!(word(&m, r.phy_table_state), 1);
    assert_eq!(word(&m, r.phy_table_at), RadioSram::ROM_DATA_START);
    let mut snapshot = [0xff; RadioSram::PHY_TABLE_WORDS as usize * 4];
    assert!(m.read_data(r.phy_table, &mut snapshot));
    let mut page = [0; RadioSram::PHY_TABLE_WORDS as usize * 4];
    assert!(m.read_data(RadioSram::ROM_DATA_START, &mut page));
    assert_eq!(snapshot, page, "the dispatcher saved the table at the published pointer, which the stand-in leaves as the model holds it");
    assert_eq!([si::SIGNATURE, si::MODE, si::STARTED, si::SEM, si::FAILED].map(|i| s.field(&m, i)),
        [si::SIM_SIGNATURE, 1, 1, 0, 0]);
    for i in [si::LOCK, si::QUEUE, si::QUEUE_HANDLE, si::TASK, si::TIMER] { assert_ne!(s.field(&m, i), 0); }
    // objects.splash: B_REQUEST is +8 in the 16-byte allocation header.
    assert_eq!(word(&m, s.field(&m, si::TIMER) - 16 + 8), 20);
    assert_eq!(word(&m, r.isrs + 4), s.entry("wDev_ProcessFiq"));
    // phy_init_done requests track only after strictly more than 1 s since the zero boot stamp.
    assert!(m.now() < m.tick_hz());
    let row = |i: u32, n| s.field(&m, si::PHY + si::PHY_WORDS * i + n);
    assert_eq!((0..10).map(|i| row(i, 0)).collect::<Vec<_>>(), [1, 1, 1, 0, 0, 0, 1, 0, 0, 0]);
    assert_eq!((row(0, 1), row(1, 1), row(6, 1)), (1, 1, 1));
    assert_ne!(row(2, 1), 0);
    assert_eq!((row(2, 2), row(2, 3)), (r.phy_cal_data, radio::kernel::PHY_RF_CAL_FULL));
    assert_eq!(s.field(&m, si::PHY_LAST), 7 | (1 << 8)); // symbol 6, on
    let mut calibration = [0; 1904];
    assert!(m.read_data(r.phy_cal_data, &mut calibration));
    assert_eq!(&calibration[..4], &[0; 4]);
    assert_eq!(&calibration[4..10], &s3_fixture::profile(53).mac);
    assert!(calibration[10..].iter().all(|&b| b == si::SIM_CAL_BYTE));
    assert_eq!([RESULT, SECOND_RESULT, MODE_OUT].map(|p| word(&m, p)), [0x102, 0, 1]);
}

fn assert_stop(m: &mut S3Machine, name: &str, reason: &str) {
    let end = m.tick_hz();
    assert_eq!(run_until(m, end), Ran::Halted);
    assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Unmodeled));
    let why = m.explain_foreign().unwrap();
    assert!(why.contains(name) && why.contains(reason), "{why}");
}

#[test]
fn caller_after_start_stops_by_name() {
    let s = station();
    for (name, arg, why) in [
        ("esp_wifi_connect_internal", 0, "not simulated yet"),
        ("esp_wifi_set_mode", 2, "a station-only pack's answer for another mode is not documented"),
        ("wDev_ProcessFiq", 0, "the Wi-Fi interrupt ran; the simulator does not raise it yet"),
    ] {
        let mut m = s.machine(call_target, s.entry(name), [arg, 0, 0]);
        reach(&mut m, CALLER);
        assert_eq!(s.field(&m, si::STARTED), 1);
        assert_stop(&mut m, name, why);
    }
}

#[test]
fn first_phy_call_must_follow_the_documented_order() {
    let s = before_init();
    let name = "register_chipv7_phy";
    let mut m = s.machine(call_target, s.entry(name), [1, s.radio().phy_cal_data, radio::kernel::PHY_RF_CAL_FULL]);
    assert_stop(&mut m, name, "calibration must follow USB BBPLL enable");
    assert_eq!(s.field(&m, si::PHY + 2 * si::PHY_WORDS), 1, "the rejected call was recorded");
}

// Two CALLX8 targets seeded before boot. Both take zero for a10.
fn two_phy_calls(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    match ctx.step {
        0 => call(TARGET, 0, 0, 0),
        1 => call(ARGS, 0, 0, 0),
        _ => park(bus),
    }
}

#[test]
fn disable_saves_the_kernel_buffer_and_wakeup_restores_it() {
    let s = station();
    let mut m = s.machine(two_phy_calls, s.island.glue["phy_disable_glue"], [s.island.glue["phy_enable_glue"], 0, 0]);
    reach(&mut m, CALLER);
    for name in ["phy_wifi_enable_set", "phy_dig_reg_backup", "phy_close_rf"] {
        reach(&mut m, s.entry(name));
    }
    let backup = si::PHY + 4 * si::PHY_WORDS;
    let saved = |m: &S3Machine| [0, 1, 2].map(|i| s.field(m, backup + i));
    assert_eq!(saved(&m), [1, 1, s.radio().phy_dig_regs]);
    for name in ["phy_xpd_tsens", "phy_wait_freq_hw_hop_done", "phy_wakeup_init",
        "phy_dig_reg_backup", "phy_wifi_enable_set"] {
        reach(&mut m, s.entry(name));
    }
    reach(&mut m, s.gate("osi_task_delay"));
    assert_eq!(saved(&m), [2, 0, s.radio().phy_dig_regs]);
}

#[test]
fn rf_close_also_accepts_wifi_off_without_a_save() {
    let s = station();
    let mut m = s.machine(two_phy_calls, s.entry("phy_wifi_enable_set"), [s.entry("phy_close_rf"), 0, 0]);
    reach(&mut m, CALLER);
    reach(&mut m, s.entry("phy_close_rf"));
    reach(&mut m, s.gate("osi_task_delay"));
    assert_eq!(s.field(&m, si::PHY + 4 * si::PHY_WORDS), 0, "no register backup");
    assert_eq!(s.field(&m, si::PHY + 7 * si::PHY_WORDS), 1, "RF closed");
}

fn clear_magic(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    if ctx.step == 0 {
        return Next::Effect(|_, b| {
            if let Some(p) = b.sram_word(MAGIC_PTR) { let _ = b.write(p, Width::W, 0, 0); }
        });
    }
    park(bus)
}

#[test]
fn cleared_config_magic_stops_before_the_first_create() {
    let s = before_init();
    let mut m = s.machine(clear_magic, s.entry("esp_wifi_init_internal"), [0; 3]);
    assert_stop(&mut m, "esp_wifi_init_internal", "config magic is not WIFI_INIT_CONFIG_MAGIC");
    assert_eq!(s.field(&m, si::LOCK), 0);
}

fn measure_free_heap(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    match ctx.step {
        0 => match bus.sram_word(FREE_SLOT) {
            Some(slot) => call(slot, 0, 0, 0),
            None => Next::Stop("missing test heap query slot"),
        },
        1 => Next::Effect(save_result::<RESULT>),
        _ => park(bus),
    }
}

#[test]
fn task_stack_refusal_releases_every_create_before_restart() {
    let s = before_init();
    let r = s.radio();
    let mut m = s.machine(measure_free_heap, s.entry("esp_wifi_init_internal"), [0; 3]);
    let init = s.entry("esp_wifi_init_internal");
    reach(&mut m, init);
    let free_before = word(&m, RESULT);
    assert_eq!(free_before, SMALL_HEAP - radio::kernel::heap_control_bytes(SMALL_HEAP) - 16);
    reach(&mut m, s.gate("osi_task_create"));
    assert_eq!(m.cpu().ar[4], si::SIM_PP_STACK);
    for field in [si::LOCK, si::QUEUE, si::SEM, si::TIMER] { assert_ne!(s.field(&m, field), 0); }
    reach(&mut m, s.gate("radio_start_failed"));
    let init_failure = (1 << 24) | 0x101;
    assert_eq!(m.cpu().ar[2], init_failure);
    assert_eq!(s.field(&m, si::FAILED), 0x101);
    for field in [si::LOCK, si::QUEUE, si::QUEUE_HANDLE, si::SEM, si::TIMER, si::TASK] {
        assert_eq!(s.field(&m, field), 0);
    }
    assert_eq!(word(&m, r.heap + 8), free_before, "heap_free_bytes reads H_FREE at +8");
    reach(&mut m, init);
    assert_eq!(word(&m, r.generation), 2);
    let end = m.now() + m.tick_hz() / 100;
    assert_eq!(run_until(&mut m, end), Ran::Tick);
    let reports = reports(&mut m);
    let restart = (1 << 24) | (i32::from(watch::FAULT_RADIO_START_FAILED) << 16) | 2;
    assert!(reports.windows(2).any(|pair| pair == [
        (watch::FAULT_RADIO_START_FAILED, init_failure as i32), (watch::FAULT_RADIO_STARTED, restart),
    ]), "{reports:?}");
}

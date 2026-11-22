#[path = "support/island_fixture.rs"]
mod island_fixture;
#[path = "../../s3/tests/support/mod.rs"]
mod s3_fixture;

use fab_elec_mcu::{compile, compile_kernel, compile_with, Compiled};
use fab_elec_mcu_emit_s3::{build_image, build_slot, chip, radio, runtime, Board, BoardTime, BootTask, Image, KernelOptions,
    KernelRegion, Layout, Radio, Slot, SlotOptions, ISLAND_MMU_PAGE, RADIO_KERNEL_LINES, SLOT_BASE,
    SLOT_DATA_BASE, SRAM_START, STACK_TOP, VOICEBOX};
use fab_elec_mcu_emit_s3::image::RestartPolicy;
use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};
use fab_elec_mcu_emu::{HaltKind, Machine, PcSet, Ran, Status, Until};
use fab_elec_mcu_island::{abi_hash, import_map::ImportClass, Archive, ImportMap, Island, IslandBinding, SegmentKind};
use fab_elec_mcu_rt::{kernel::RegionKind, module, watch};
use fab_elec_mcu_s3::{config::s3_config, S3Machine};
use fab_elec_mcu_xtensa::insn::{decode, Insn, Op};
use fab_elec_mcu_xtensa::sim::{Cpu, Stop};
use std::collections::BTreeMap;

fn app(source: &str) -> Compiled { compile(source, &chip::profile(), &[], &[]).unwrap() }

// D0..=D21 and D26..=D48 name their GPIO numbers, as in tests/runtime.rs.
fn pin_app(source: &str) -> Compiled {
    let names: Vec<_> = (0..=21).chain(26..=48).map(|p| (format!("D{p}"), p)).collect();
    let pins: Vec<_> = names.iter().map(|(name, pin)| (name.as_str(), *pin)).collect();
    compile_with(source, &chip::profile(), &pins, &[], &[]).unwrap_or_else(|e| panic!("{e:?}\n{source}"))
}

const FIXTURE_HEAP_BYTES: u32 = 4096;

fn profile(island: IslandBinding) -> Radio {
    Radio { island, start: None, restart: RestartPolicy::PROVISIONAL, dispatcher_priority: 1, irq_stack_bytes: radio::MIN_STACK_BYTES, task_capacity: 2,
        isr_capacity: radio::PACK_ISR_LINES, heap_bytes: FIXTURE_HEAP_BYTES, timer_reserve: 8,
        trace_entries: 0, burst_insns: 0, boot_tasks: vec![] }
}

fn board(radio: Radio) -> Board {
    Board { radio: Some(radio), layout: VOICEBOX, panel: None, i2c: None, motion: None, touch: None,
        time: BoardTime { rc_slow_hz: (136_000, 1), wdt_delay_sel: 0 } }
}

#[test]
fn production_station_map_links_and_binds_the_kernel() {
    use std::collections::BTreeSet;

    let map = ImportMap::parse(include_str!("../../island/packs/esp32s3-idf-v6.1.1.imports")).unwrap();
    let mapped: BTreeSet<_> = map.entries.iter().filter_map(|(name, class)| {
        (class == &ImportClass::Glue).then_some(name.as_str())
    }).collect();
    let objects = radio::island_glue();
    let start = objects.iter().find(|g| g.name == "radio_start").unwrap();
    // Five literals, ENTRY/MOV, three MOVI/L32R/CALLX8/BNEZ groups, one mode
    // argument, J, four error-packing instructions, failure call, and the park loop.
    assert_eq!((start.entry, start.bytes.len()), (20, 20 + 5 + 3 * 12 + 3 + 3 + 12 + 6 + 12));
    let names: BTreeSet<_> = objects.iter().map(|g| g.name.as_str()).collect();
    assert_eq!(names.len(), objects.len(), "each glue object has a unique name");
    assert_eq!(names, mapped, "the map and the production glue supply exactly the same names");
    for name in radio::helpers::HELPERS {
        assert_eq!(map.entries.get(name), Some(&ImportClass::Glue), "{name} is island glue");
    }

    let (roots, archive) = island_fixture::station_archive(vec![1, 3, 5], 17);
    let island = radio::link_pack(&[Archive { name: "station.a", data: &archive }]).expect("link the production contract");
    assert_eq!(island.root_names, roots, "the linked entries preserve station root order");
    let gates = ["free", "phy_lock", "phy_enter", "phy_init_done", "phy_leave",
        "puts", "net80211_printf", "pp_printf", "phy_printf", "malloc", "event_commit", "radio_start_failed", "osi_task_delay"];
    assert_eq!(island.gate_names, gates, "indexed gate names are part of the station ABI");
    let app = app("fn setup() {}");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let image = build_image(&module, &board(profile(island.binding())), &SlotOptions::default())
        .expect("the voicebox image serves the production binding");
    let symbols = image.slot.symbols.radio.as_ref().unwrap();
    for name in gates {
        assert!(symbols.gate_reports.iter().any(|gate| gate.name == name), "kernel export for {name}");
    }

    let bytes = |name: &str, kind: SegmentKind, len: u32| {
        let address = island.glue[name];
        let segment = island.segments.iter().find(|s| s.kind == kind).unwrap();
        assert!(address >= segment.address && address - segment.address + len <= segment.bytes,
            "{name} fits in its {} segment", kind.name());
        let offset = address - segment.address;
        let start = (segment.file_offset + offset) as usize;
        &island.image[start..start + len as usize]
    };
    for word in radio::ftm_words::glue() {
        assert_eq!(bytes(&word.name, SegmentKind::Data, 4), word.bytes.as_slice(), "{} placed in DATA", word.name);
    }
    assert_eq!(bytes("WIFI_EVENT", SegmentKind::Rodata, 4), island.glue["wifi_event_base_name"].to_le_bytes());
    let event_name = b"WIFI_EVENT\0";
    assert_eq!(bytes("wifi_event_base_name", SegmentKind::Rodata, event_name.len() as u32), event_name);
    for name in ["regdomain_table", "regulatory_data"] {
        assert!(island.glue.contains_key(name), "linked regulatory object {name}");
    }
}

#[test]
fn island_window_refuses_app_that_otherwise_fits() {
    // Cross the difference between the app's pool and physical SRAM without adding flash bytes.
    let island = island_fixture::linked_island(vec![1, 3, 5], 32 * 1024 + 1);
    let options = SlotOptions { radio: Some(profile(island.binding())), ..SlotOptions::default() };
    let with_buffer = |n| app(&format!("var count: i32 = 1\nlet payload = bytes({n})\nfn setup() {{ count += 1 }}"));
    let probe = with_buffer(1u32);
    let module = module::load(&probe.module, &chip::profile()).unwrap();
    let layout = build_slot(&module, &options).unwrap().layout;
    let available = STACK_TOP - layout.t0;
    assert!(available < chip::profile().ram_words * 4, "the physical stack top limits this window");
    let state = layout.state_words * 4;
    let capacity = available - layout.runtime_bytes - state - layout.stack_bytes;
    assert!(layout.stack_bytes >= radio::DISPATCHER_HEADROOM + 4, "one word over still fits before handler sizing");

    let exact = with_buffer(capacity - 1);
    let exact = module::load(&exact.module, &chip::profile()).unwrap();
    let fit = build_slot(&exact, &options).expect("word padding and the largest handler stack fit exactly").layout;
    assert_eq!(fit.buffers[0].allocated_bytes, u64::from(capacity));
    assert_eq!(fit.regions[0].used + u64::from(fit.stack_bytes), u64::from(STACK_TOP));

    let over = with_buffer(capacity + 1);
    let over = module::load(&over.module, &chip::profile()).unwrap();
    assert_eq!(build_slot(&over, &SlotOptions::default()).unwrap().layout.t0, SRAM_START);
    let message = build_slot(&over, &options).err().expect("the same module exceeds the bound image").message;
    let r = fit.radio.as_ref().unwrap();
    let radio_bytes = r.end - r.start;
    for field in [
        format!("window {SRAM_START:#010x}..{:#010x} ({} bytes)", island.window_end, island.window_end - SRAM_START),
        format!("alignment padding {} bytes", fit.t0 - island.window_end),
        format!("runtime {} bytes", r.start - fit.t0),
        format!("radio {radio_bytes} bytes"), format!("state {state} bytes"),
        format!("padded buffers {} bytes", capacity + 4),
        format!("stack reservation {} bytes", fit.stack_bytes),
        format!("available {available} bytes"), "deficit 4 bytes".into(),
    ] { assert!(message.contains(&field), "missing {field} in {message}"); }

    // Fail while placing the radio itself. Its kernel state and the app's later allocations
    // still belong in the accounting; this pass has only reserved dispatcher headroom.
    let mut oversized = options;
    oversized.radio.as_mut().unwrap().irq_stack_bytes += available;
    let message = build_slot(&exact, &oversized).err().expect("radio allocation exceeds physical SRAM").message;
    let deficit = available + radio::DISPATCHER_HEADROOM - fit.stack_bytes;
    for field in [format!("radio {} bytes", radio_bytes + available), format!("padded buffers {capacity} bytes"),
        format!("available {available} bytes"), format!("deficit {deficit} bytes")] {
        assert!(message.contains(&field), "missing {field} in {message}");
    }
}

#[test]
fn image_refuses_a_binding_that_does_not_match() {
    let good = island_fixture::small_island().binding();
    let app = app("fn setup() {}");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let refused = |binding, reason: &str| {
        let radio = profile(binding);
        let slot = build_slot(&module, &SlotOptions { radio: Some(radio.clone()), ..SlotOptions::default() });
        let image = build_image(&module, &board(radio), &SlotOptions::default());
        for message in [slot.err().expect("standalone slot refused").message, image.err().expect("image refused").message] {
            assert!(message.contains(reason), "expected {reason} in {message}");
        }
    };
    // These two manifests are internally consistent. Only the image's call-in
    // requirements and the compiled kernel's actual exports can refuse them.
    let hash_names = |b: &IslandBinding| {
        let roots: Vec<_> = b.entries.iter().map(|(name, _)| name.clone()).collect();
        let gates: Vec<_> = b.gate_names.iter().map(String::as_str).collect();
        abi_hash(&roots, &gates)
    };
    let mut wrong = good.clone();
    wrong.header.mmu_page -= 1;
    refused(wrong, &format!("mmu_page {}, board requires {ISLAND_MMU_PAGE}", ISLAND_MMU_PAGE - 1));
    let mut wrong = good.clone();
    wrong.objects.remove(radio::PHY_FUNS_OBJECT);
    refused(wrong, "missing required object g_phyFuns");
    let mut wrong = good.clone();
    wrong.objects.get_mut(radio::PHY_FUNS_OBJECT).unwrap().bytes = 8;
    refused(wrong, "object g_phyFuns is 8 bytes, expected 4");
    let mut wrong = good.clone();
    let (name, _) = wrong.entries.remove(0);
    wrong.header.entry_count -= 1;
    wrong.header.abi_hash = hash_names(&wrong);
    refused(wrong, &format!("missing required root {name}"));
    let mut wrong = good.clone();
    wrong.gate_names.swap(0, 1);
    refused(wrong, "gate order differs");
    let mut wrong = good.clone();
    wrong.gate_names[0] = "unknown_gate".into();
    wrong.header.abi_hash = hash_names(&wrong);
    refused(wrong, "gate 0 (unknown_gate) has no kernel export");
    let mut wrong = good.clone();
    wrong.header.gate_table += 16;
    refused(wrong, "window starts at 0x3fc88010, board requires 0x3fc88000");
    let mut wrong = good.clone();
    wrong.window_end += 4;
    refused(wrong, "linked extents require");

    let oversized = Radio { timer_reserve: 64, ..profile(good.clone()) };
    let error = build_image(&module, &board(oversized), &SlotOptions::default()).err().unwrap();
    assert!(error.message.contains("368 control bytes + 3856 timer floor bytes + 32 minimum block bytes"), "{}", error.message);

    let mut start = Radio { start: Some(fab_elec_mcu_emit_s3::image::StartTask { priority: 2, stack_bytes: 4096 }),
        ..profile(good.clone()) };
    let error = build_slot(&module, &SlotOptions { radio: Some(start.clone()), ..SlotOptions::default() }).err().unwrap();
    assert!(error.message.contains("task_capacity must hold dispatcher, idle and every boot task"));
    start.task_capacity = 3;
    start.island.glue.remove("radio_start");
    let error = build_slot(&module, &SlotOptions { radio: Some(start), ..SlotOptions::default() }).err().unwrap();
    assert!(error.message.contains("missing radio_start glue"));

    let mut smaller = board(profile(good));
    smaller.layout.island_pages -= 1;
    let error = build_image(&module, &smaller, &SlotOptions::default()).err().unwrap();
    assert!(error.message.contains(&format!("board requires {}", ISLAND_MMU_PAGE + 1)));
    for (pages, reason) in [(513, "512-entry MMU table"), (449, "ROM mapping"), (420, "slot mapping"), (400, "between the slot's and the island's")] {
        assert!(Layout { island_pages: pages, ..VOICEBOX }.check().unwrap_err().contains(reason));
    }
}

#[test]
fn two_apps_bind_the_same_fixture_island() {
    let island = island_fixture::small_island();
    let mut board = board(profile(island.binding()));
    let expected = &board.radio.as_ref().unwrap().island;
    assert_eq!(expected.header, island.header);
    for (i, (name, address)) in expected.entries.iter().enumerate() {
        let offset = island.header.entry_offset as usize + i * 4;
        assert_eq!(name, &island.root_names[i]);
        assert_eq!(address.to_le_bytes().as_slice(), &island.image[offset..offset + 4]);
    }
    let build = |app: &Compiled, board: &Board| {
        let module = module::load(&app.module, &chip::profile()).unwrap();
        build_image(&module, board, &SlotOptions::default()).unwrap()
    };
    let setup = app("fn setup() {}");
    let small = build(&setup, &board);
    let timed = build(&app("var ticks: i32 = 1\nlet samples_out = bytes(17)\nlet tick = every(ms(5), fn() { ticks += 1 })"), &board);
    for image in [&small, &timed] {
        let layout = &image.slot.layout;
        let r = layout.radio.as_ref().unwrap();
        assert!(r.wifi_config.is_none());
        assert_eq!(r.island_guard, island.guard);
        assert_eq!(layout.t0, island.window_end.next_multiple_of(16));
        assert!(layout.state >= layout.t0 && layout.state >= island.window_end);
        let m = boot_past_island_start(image, &island.image);
        assert_eq!(ram_word(&m, r.island_status), radio::ISLAND_READY);
        assert_eq!(ram_word(&m, r.island_guard), radio::GUARD);
    }
    assert_ne!(small.slot.layout.state, timed.slot.layout.state, "the two apps place different runtime data above the same island");
    assert_eq!(small.slot.layout.buffers.len() + 1, timed.slot.layout.buffers.len());

    board.radio = None;
    let plain = build(&setup, &board);
    board.layout.island_pages -= 1;
    let control = build(&setup, &board);
    assert_eq!(plain.slot.layout.t0, SRAM_START);
    assert_eq!(control.slot.layout.t0, SRAM_START);
    assert!(plain.flash == control.flash, "the island reservation has no effect on any radio-off image byte");
}

fn planned_slot() -> &'static (Radio, Slot, Vec<u8>) {
    static FIXTURE: std::sync::OnceLock<(Radio, Slot, Vec<u8>)> = std::sync::OnceLock::new();
    FIXTURE.get_or_init(|| {
        let island = island_fixture::small_island();
        let radio = Radio { start: None, ..Radio::voicebox(island.binding()) };
        let app = app("fn setup() {}");
        let module = module::load(&app.module, &chip::profile()).unwrap();
        let image = build_image(&module, &board(radio.clone()), &SlotOptions::default()).unwrap();
        (radio, image.slot, island.image.clone())
    })
}

const IRAM_START: u32 = 0x4037_8000;
const RW: Perm = Perm { read: true, write: true, exec: false };
const RX: Perm = Perm { read: true, write: false, exec: true };

fn slot_memory(slot: &Slot, heap_bytes: u32) -> (Memory, usize) {
    let mut memory = Memory::new();
    let flash = memory.store(slot.bytes.len());
    memory.bytes_mut(flash).copy_from_slice(&slot.bytes);
    memory.map(SLOT_BASE, flash, 0, slot.bytes.len(), RX);
    memory.map(SLOT_DATA_BASE, flash, 0, slot.bytes.len(), Perm { exec: false, ..RX });
    let len = (STACK_TOP - SRAM_START) as usize;
    let ram = memory.store(len);
    memory.map(SRAM_START, ram, 0, len, RW);
    memory.map(IRAM_START, ram, 0, len, RW);
    let heap = (slot.layout.radio.as_ref().unwrap().heap - SRAM_START) as usize;
    assert!(memory.bytes(ram)[heap..heap + heap_bytes as usize].iter().all(|&b| b == 0),
        "slot_memory must supply the zeroed heap required by objects.splash");
    (memory, ram)
}

fn measure_gate(slot: &Slot, memory: &mut impl Bus, name: &str, args: &[u32]) -> (Result<u32, u32>, u64) {
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let gate = symbols.gate_reports.iter().find(|g| g.name == name).unwrap();
    let mut cpu = Cpu::new(s3_config(), gate.address);
    cpu.ps = 15;
    cpu.ar[0] = SLOT_BASE;
    cpu.ar[1] = STACK_TOP;
    cpu.ar[2..2 + args.len()].copy_from_slice(args);
    for _ in 0..gate.max_insns {
        match cpu.step(memory) {
            Ok(()) if cpu.pc == SLOT_BASE => return (Ok(cpu.ar[2]), cpu.instret),
            Ok(()) => {}
            Err(Stop::Exception { cause: 0, pc, .. }) => {
                let fault = symbols.kernel_symbols.iter().find(|s| s.0 == "kernel_fault").unwrap();
                assert_eq!(pc, fault.1, "{name} must fault at the kernel's checked-access trap");
                return (Err(cpu.ar[2]), cpu.instret);
            }
            Err(stop) => panic!("{name} stopped at {stop:?}"),
        }
    }
    panic!("{name} exceeded its reported bound");
}

#[test]
fn report_publication_and_stop_comparison_path_lengths() {
    let (radio, slot, _) = planned_slot();
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let trap = s.kernel_symbols.iter().find(|sym| sym.0 == "kernel_fault").unwrap().1;
    let (mut memory, ram) = slot_memory(slot, radio.heap_bytes);
    let empty = slot.map.runtime.iter().find(|routine| routine.name == "radio_events").unwrap().start;
    let mut cpu = Cpu::new(s3_config(), empty);
    cpu.ps = 15; cpu.ar[0] = SLOT_BASE;
    for _ in 0..5 { cpu.step(&mut memory).unwrap(); }
    assert_eq!(cpu.pc, SLOT_BASE, "empty event list check takes five instructions plus the two-instruction call");
    let offset = |addr| (addr - SRAM_START) as usize;
    let word = |m: &Memory, addr| u32::from_le_bytes(m.bytes(ram)[offset(addr)..offset(addr) + 4].try_into().unwrap());
    for (code, stop_comparisons) in [(100, 2), (107, 3), (110, 4), (113, 0), (114, 0)] {
        for (tail, lost, publish) in [(0, 0u32, 15), (radio::REPORT_RING_ENTRIES, 0, 11),
            (radio::REPORT_RING_ENTRIES, u32::MAX, 9)] {
            for (addr, value) in [(r.reports, 0), (r.reports + 4, tail), (r.reports + 8, lost), (r.stop_pending, 0)] {
                memory.bytes_mut(ram)[offset(addr)..offset(addr) + 4].copy_from_slice(&value.to_le_bytes());
            }
            let mut cpu = Cpu::new(s3_config(), s.report);
            cpu.ps = 15; cpu.ar[0] = SLOT_BASE; cpu.ar[2] = code; cpu.ar[3] = 42; cpu.ar[10] = 9;
            // The stopping suffix retires five instructions before faulting at the shared ILL.
            let retired = publish + if stop_comparisons == 0 { 5 } else { stop_comparisons + 5 };
            for _ in 0..retired { cpu.step(&mut memory).unwrap(); }
            assert_eq!(cpu.instret, retired);
            assert_eq!(cpu.pc, if stop_comparisons == 0 { SLOT_BASE } else { trap });
            assert_eq!(word(&memory, r.stop_pending), if stop_comparisons == 0 { 0 } else { code });
            if tail == 0 {
                assert_eq!([4, 12, 16, 20].map(|n| word(&memory, r.reports + n)), [1, code, 9, 42]);
            } else {
                assert_eq!(word(&memory, r.reports + 8), lost.saturating_add(1));
            }
        }
    }
}

// A fixed unit-0 snapshot lets all the later timers share a deadline. UPDATE makes it VALID.
const SYSTIMER: u32 = 0x6002_3000;
struct TimerClock<'a>(&'a mut Memory);

impl Bus for TimerClock<'_> {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        self.0.read(addr, width, kind, now)
    }
    fn write(&mut self, addr: u32, width: Width, mut value: u32, now: u64) -> Result<(), ()> {
        if addr == SYSTIMER + 4 {
            assert_eq!((width, value), (Width::W, 1 << 30), "timer probe must request a unit-0 snapshot");
            value |= 1 << 29;
        }
        self.0.write(addr, width, value, now)
    }
}

fn measure_pending_arm(radio: &Radio, slot: &Slot, memory: &mut Memory) -> u64 {
    const ETS_BYTES: u32 = 20;
    let registers = memory.store(0x48);
    memory.map(SYSTIMER, registers, 0, 0x48, RW);
    let mut bus = TimerClock(memory);
    let call = |bus: &mut TimerClock<'_>, name: &str, args: &[u32]| {
        measure_gate(slot, bus, name, args).0.unwrap_or_else(|fault| panic!("{name} faulted: {fault}"))
    };
    let peers = radio.timer_reserve;
    let timers = call(&mut bus, "malloc", &[(peers + 1) * ETS_BYTES + 16]);
    assert_ne!(timers, 0, "timer probe needs pack-visible timers and a kernel deadline output");
    let pending = timers + peers * ETS_BYTES;
    let out = pending + ETS_BYTES;
    let free = call(&mut bus, "heap_free_bytes", &[]);
    let floor = radio::kernel::timer_floor_bytes(peers) as u32;
    let held = call(&mut bus, "malloc", &[free - floor - 16]);
    assert_ne!(held, 0, "holding the free payload above the reserve must succeed");
    let callback = radio.island.entries[0].1;
    for i in 0..peers {
        let ets = timers + i * ETS_BYTES;
        assert_eq!(call(&mut bus, "timer_setfn", &[ets, callback, ets]), 1, "reserved timer {i} gets a node");
        assert_eq!(call(&mut bus, "timer_arm_us", &[ets, 1000, 0]), 1, "timer {i} enters the heap with a later deadline");
    }
    assert_eq!(call(&mut bus, "timer_setfn", &[pending, callback, pending]), 0, "the next timer must become pending");
    call(&mut bus, "free", &[held]);
    let (result, span) = measure_gate(slot, &mut bus, "timer_arm", &[pending, 0, 0]);
    assert_eq!(result, Ok(1), "pending arm creates the node and arms it");
    assert_eq!(call(&mut bus, "timer_due", &[out]), 1, "the recovered timer is due ahead of all its peers");
    assert_eq!(bus.read(out + 4, Width::W, Kind::Load, 0).unwrap(), pending, "the recovered timer sifted to the root");
    span
}

fn measure_reaping_create(radio: &Radio, slot: &Slot, memory: &mut Memory) -> u64 {
    let r = slot.layout.radio.as_ref().unwrap();
    let symbols = slot.symbols.radio.as_ref().unwrap();
    // Install the dispatcher's and idle task's table entries as at boot.
    for (at, value) in [(r.tasks, r.dispatcher), (r.tasks + 4, r.idle), (r.task_count, 2),
        (r.current, r.dispatcher), (r.dispatcher + radio::TCB_PRIORITY, radio.dispatcher_priority),
        (symbols.task_exit_link, radio::windowed_exit_link(symbols.task_exit))] {
        memory.write(at, Width::W, value, 0).unwrap();
    }
    let out = measure_gate(slot, memory, "malloc", &[4]).0.unwrap();
    assert_ne!(out, 0, "task probe needs a writable handle word");
    let args = [radio.island.entries[0].1, 0, radio::MIN_STACK_BYTES, 0, radio.dispatcher_priority + 1, out];
    for _ in 2..radio.task_capacity {
        assert_eq!(measure_gate(slot, memory, "task_create", &args).0, Ok(1), "populate the task table up to capacity");
    }
    let zombie = memory.read(out, Width::W, Kind::Load, 0).unwrap();
    memory.write(r.current, Width::W, zombie, 0).unwrap();
    assert_eq!(measure_gate(slot, memory, "task_delete", &[0]).0, Ok(1), "the last task self-deletes on its own stack");
    assert_eq!([zombie + radio::TCB_STATE, r.task_count].map(|at| memory.read(at, Width::W, Kind::Load, 0).unwrap()),
        [radio::BLOCKED, radio.task_capacity], "self-delete leaves the zombie in the full table for the measured create");
    memory.write(r.current, Width::W, r.dispatcher, 0).unwrap();
    let (result, span) = measure_gate(slot, memory, "task_create", &args);
    assert_eq!(result, Ok(1), "create must reap the zombie, allocate both blocks and initialize the new task");
    assert_eq!(memory.read(r.task_count, Width::W, Kind::Load, 0).unwrap(), radio.task_capacity,
        "the replacement fills the reclaimed task slot");
    span
}

fn fixture_kernel(radio: &Radio, slot: &Slot) -> Compiled {
    use radio::kernel;
    let r = slot.layout.radio.as_ref().unwrap();
    let layout = kernel::KernelLayout { current: r.current, tasks: r.tasks, task_count: r.task_count,
        max_tasks: radio.task_capacity, heap_base: r.heap, timer_reserve: radio.timer_reserve,
        heap_size: radio.heap_bytes, code_region: SLOT_BASE >> 30, isrs: r.isrs, isr_count: r.isr_count,
        isr_capacity: radio.isr_capacity, phy_cal_data: r.phy_cal_data, phy_dig_regs: r.phy_dig_regs, phy_mutex: r.phy_mutex };
    compile_kernel(kernel::OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES, &kernel::kernel_consts(&layout)).unwrap()
}

fn report_region_cost(radio: &Radio, slot: &Slot, previous_regions: Vec<KernelRegion>, retired: &BTreeMap<&str, u64>) {
    use radio::kernel::{self, gates::{gates, EntryRegs, GateReport, EXIT_LEVEL_EXPORTS}};
    let r = slot.layout.radio.as_ref().unwrap();
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let compiled = fixture_kernel(radio, slot);
    let m = module::load_kernel(&compiled.module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let base = symbols.blobs.iter().find(|b| b.name == "radio_objects").unwrap().start;
    let mut options = KernelOptions { base, state: r.kernel_state, t0: slot.layout.t0,
        reports: Some(r.reports), stop_pending: Some(r.stop_pending), regions: previous_regions };
    let before = kernel::build_kernel(&m, &options).unwrap();
    // Gate wrappers use fixed-width literals for both entry registers.
    let before = gates((base + before.bytes.len() as u32).next_multiple_of(4), &before,
        EntryRegs { state: r.kernel_state, shared: slot.shared });
    let cost = |g: &GateReport| g.max_insns + radio::masked_insns(g.blocks,
        EXIT_LEVEL_EXPORTS.contains(&g.name.as_str()), radio.trace_entries != 0);
    let old = before.gates.iter().max_by_key(|g| cost(g)).unwrap();
    let old_masked = cost(old);
    let mut ranked: Vec<_> = symbols.gate_reports.iter().map(|gate| (cost(gate), gate)).collect();
    ranked.sort_by_key(|&(masked, _)| masked);
    let &(masked, new) = ranked.last().unwrap();
    assert_eq!(masked, symbols.masked_gate_insns, "the image charges the largest gate and its masked thunk");
    options.regions = symbols.kernel_regions.iter().map(|(range, writable, kind)|
        KernelRegion { base: range.start, len: range.end - range.start, writable: *writable, kind: *kind }).collect();
    let checked = kernel::checked_gate_bounds(&m, &options, slot.shared).unwrap();
    assert_eq!(checked.len(), symbols.gate_reports.len(), "one checked-call comparison per production gate");
    let mut largest_before = (0, "");
    eprintln!("| Gate | Masked before | Masked after | Gain % | Largest observed gate span (retired) | Gate bound / span | Folded K | Folded P | Folded D |");
    eprintln!("| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |");
    for ((name, bound), gate) in checked.iter().zip(&symbols.gate_reports) {
        use fab_elec_mcu_rt::kernel::AccessClass::{Device, Kernel, Pack};
        assert_eq!(name, &gate.name, "compare the same export at the same layout");
        let after = cost(gate);
        let before = *bound + (after - gate.max_insns);
        if before > largest_before.0 { largest_before = (before, name.as_str()); }
        let [k, p, d] = [Kernel, Pack, Device].map(|class|
            gate.proven_accesses.iter().filter(|site| site.class == class).count());
        let observed = retired.get(name.as_str()).map_or_else(|| "—".into(), u64::to_string);
        let ratio = retired.get(name.as_str()).map_or_else(|| "—".into(), |&span|
            format!("{} / {span} = {:.2}x", gate.max_insns, gate.max_insns as f64 / span as f64));
        let gain = 100.0 * (1.0 - after as f64 / before as f64);
        eprintln!("| {name} | {before} | {after} | {gain:.2} | {observed} | {ratio} | {k} | {p} | {d} |");
    }
    let gain = 100.0 * (1.0 - masked as f64 / largest_before.0 as f64);
    eprintln!("largest masked gate before {} = {}, after {} = {masked}, gain {gain:.2}% (c2 threshold 5%)",
        largest_before.1, largest_before.0, new.name);
    assert!(masked > old_masked, "checking the island increases the largest planned bound");
    eprintln!("planned image before island regions: {} gate {}, masked {old_masked}; after: {} gate {}, masked {masked} (+{})",
        old.name, old.max_insns, new.name, new.max_insns, masked - old_masked);
    eprintln!("| Image gate | Gate max_insns | Masked insns | Port calls | Port insns | Walk insns | Walk % of masked |");
    eprintln!("| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    for (rank, &(masked, gate)) in ranked.iter().rev().enumerate() {
        let (mut calls, mut insns, mut walk) = (0u64, 0u64, 0u64);
        for charge in &gate.port_charges {
            assert!(charge.calls > 0, "{} {}: a listed port has calls", gate.name, charge.name());
            assert!(charge.walk <= charge.insns, "{} {}: walk fits in port charge", gate.name, charge.name());
            calls += charge.calls;
            insns += charge.insns;
            walk += charge.walk;
        }
        assert!(insns <= gate.max_insns, "{}: port charges fit in gate bound", gate.name);
        if rank < 5 {
            let share = 100.0 * walk as f64 / masked as f64;
            eprintln!("| {} | {} | {masked} | {calls} | {insns} | {walk} | {share:.1}% |", gate.name, gate.max_insns);
        }
    }
    eprintln!("| Image gate | Port | Calls | Insns | Walk insns |\n| --- | --- | ---: | ---: | ---: |");
    for &(_, gate) in ranked.iter().rev().take(2) {
        for charge in &gate.port_charges {
            eprintln!("| {} | {} | {} | {} | {} |", gate.name, charge.name(), charge.calls, charge.insns, charge.walk);
        }
    }
}

#[test]
fn kernel_checks_island_memory_by_segment() {
    use radio::kernel::{EFUSE_MAC_0, EFUSE_MAC_1, INTR_MATRIX, RNG_DATA, RTC_STORE1,
        SYSCON_WIFI_CLK_EN, SYSCON_WIFI_RST_EN, WIFI_SOURCES};
    let (radio, slot, image) = planned_slot();
    let island = &radio.island;
    let segment = |kind| island.segments.iter().find(|s| s.kind == kind).unwrap();
    let (data, bss, iram) = (segment(SegmentKind::Data), segment(SegmentKind::Bss), segment(SegmentKind::Iram));
    let drom = 0x3c00_0000 + island.header.mmu_page * 65536;
    let iram_alias = SRAM_START + (iram.address - IRAM_START);
    let regions = &slot.symbols.radio.as_ref().unwrap().kernel_regions;
    let sram = slot.layout.radio.as_ref().unwrap();
    assert_eq!(regions.as_slice(), &[
        (sram.start..sram.end, true, RegionKind::Kernel),
        (SYSCON_WIFI_CLK_EN..SYSCON_WIFI_RST_EN + 4, true, RegionKind::Device),
        (INTR_MATRIX..INTR_MATRIX + WIFI_SOURCES * 4, true, RegionKind::Device),
        (EFUSE_MAC_0..EFUSE_MAC_1 + 4, false, RegionKind::Device),
        (RTC_STORE1..RTC_STORE1 + 4, false, RegionKind::Device),
        (RNG_DATA..RNG_DATA + 4, false, RegionKind::Device),
        (data.address..bss.address + bss.bytes, true, RegionKind::Pack),
        (drom..drom + island.header.image_size, false, RegionKind::Pack),
    ], "production probe order, ranges, write permissions and region kinds");
    for protected in [SRAM_START..SRAM_START + 4 * island.header.gate_count, iram.address..iram.address + iram.bytes,
        iram_alias..iram_alias + iram.bytes, island.guard..island.guard + 4] {
        assert!(regions.iter().all(|(r, _, _)| r.end <= protected.start || protected.end <= r.start),
            "no checked region may intersect {protected:?}");
    }

    let (mut memory, ram) = slot_memory(slot, radio.heap_bytes);
    let flash = memory.store(image.len());
    memory.bytes_mut(flash).copy_from_slice(image);
    // Give the bus write access even at the protected addresses so only the kernel can refuse them.
    memory.map(drom, flash, 0, image.len(), RW);
    let efuse = memory.store(8);
    memory.bytes_mut(efuse).copy_from_slice(&[6, 5, 4, 3, 2, 1, 0, 0]);
    memory.map(radio::kernel::EFUSE_MAC_0, efuse, 0, 8, Perm { exec: false, ..RX });
    let mut retired = BTreeMap::<&'static str, u64>::new();
    let mut call_gate = |slot: &Slot, mem: &mut Memory, name: &'static str, args: &[u32]| {
        let (result, count) = measure_gate(slot, mem, name, args);
        let largest = retired.entry(name).or_default();
        *largest = (*largest).max(count);
        result
    };
    assert_eq!(call_gate(slot, &mut memory, "osi_read_mac", &[bss.address, 0]), Ok(0));
    let offset = (bss.address - SRAM_START) as usize;
    assert_eq!(&memory.bytes(ram)[offset..offset + 6], &[1, 2, 3, 4, 5, 6], "six checked stores reached BSS");
    assert_eq!(call_gate(slot, &mut memory, "sem_count", &[drom]), Err(u32::from(watch::FAULT_KERNEL_OUT_OF_REGION)),
        "sem_count uses a K load, which cannot reach island DROM");
    const ITEM_BYTES: usize = 12; // Includes the header-size word at offset eight.
    let queue = call_gate(slot, &mut memory, "queue_create", &[1, ITEM_BYTES as u32]).expect("queue_create gate returns");
    assert_ne!(queue, 0, "the radio heap holds a one-item queue");
    assert_eq!(call_gate(slot, &mut memory, "queue_send_back", &[queue, drom, 0]), Ok(1), "P copy reads the DROM header");
    assert_eq!(call_gate(slot, &mut memory, "queue_recv", &[queue, bss.address, 0]), Ok(1), "P copy writes the item to BSS");
    assert_eq!(&memory.bytes(ram)[offset..offset + ITEM_BYTES], &island.header.encode()[..ITEM_BYTES],
        "the queue delivers the first twelve DROM header bytes to BSS");
    for addr in [island.header.gate_table, iram.address, iram_alias, island.guard, drom] {
        assert_eq!(call_gate(slot, &mut memory, "osi_read_mac", &[addr, 0]), Err(u32::from(watch::FAULT_KERNEL_OUT_OF_REGION)),
            "kernel refused a write to {addr:#010x}");
    }
    retired.insert("timer_arm", measure_pending_arm(radio, slot, &mut memory));
    retired.insert("task_create", measure_reaping_create(radio, slot, &mut memory));
    let previous = regions.iter().filter(|(_, _, kind)| *kind != RegionKind::Pack)
        .map(|(r, w, kind)| KernelRegion { base: r.start, len: r.end - r.start, writable: *w, kind: *kind }).collect();
    report_region_cost(radio, slot, previous, &retired);
}

#[test]
fn event_gates_fit_the_base_planned_masked_bound() {
    use fab_elec_mcu_rt::ports::{LOAD32, STORE32};
    use radio::kernel::gates::{GateReport, EXIT_LEVEL_EXPORTS};

    let (profile, slot, _) = planned_slot();
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let cost = |g: &GateReport| g.max_insns + radio::masked_insns(g.blocks,
        EXIT_LEVEL_EXPORTS.contains(&g.name.as_str()), profile.trace_entries != 0);
    let gate_named = |name: &str| symbols.gate_reports.iter().find(|g| g.name == name).unwrap();
    // Both longest paths do one checked access: alignment 5, range probe 5,
    // range fault 3, load/store and RET 2, CALLX0 and ILL 2 = 17. The windowed
    // thunk costs 9. Each CALL0 wrapper costs 11 + 2 per argument. Entry/clear
    // costs 32 + entry_stack_bytes - 16 = gate.stack_bytes. All remaining
    // instructions come from the body below. Any inlined return loop has one trip.
    for (name, port, wrapper) in [("event_commit", STORE32, 13), ("event_take", LOAD32, 11)] {
        let g = gate_named(name);
        assert!(!g.blocks);
        let [word] = g.port_charges.as_slice() else { panic!("{name} must charge one word access") };
        assert_eq!((word.port, word.calls, word.walk, word.insns), (port, 1, 5, 17));
        let index = symbols.gate_reports.iter().position(|report| report.name == name).unwrap();
        let map = &symbols.kernel_map;
        let ops = || map.ops.iter().filter(|op| op.entry == index);
        let checked = ops().find(|op| op.site.is_some()).unwrap().start;
        let mut pc = ops().next().unwrap().start;
        let end = ops().chain(map.loops.iter().filter(|op| op.entry == index)).map(|op| op.end).max().unwrap();
        let mut body = 0u64;
        while pc < end {
            let at = (pc - SLOT_BASE) as usize;
            let bits = u32::from_le_bytes(slot.bytes[at..at + 4].try_into().unwrap());
            let (i, len) = decode(bits).unwrap();
            let next = pc + len;
            // Commit's checked store is in the else arm. Count that arm and skip
            // the empty-list assignment. Take's two ifs both have empty else arms.
            pc = if name == "event_commit" && pc < checked && matches!(i.op, Op::Bnez | Op::BnezN | Op::Beqz | Op::BeqzN) {
                fab_elec_mcu_xtensa::insn::target(&i, pc).filter(|&dst| next < dst && dst <= checked).unwrap_or(next)
            } else { next };
            body += 1;
        }
        assert_eq!(pc, end);
        // The decoded body includes CALLX0. The checked-access charge includes it too.
        let masked = 9 + wrapper + u64::from(g.stack_bytes) + body + word.insns - 1;
        assert_eq!(masked, cost(g), "{name} longest arm, frame, wrapper, checked access and masked thunk");
        eprintln!("{name}: {masked} masked = 9 thunk + {wrapper} wrapper + {} frame + {body} body + 16 access", g.stack_bytes);
        assert!(masked < 15_801);
    }
    assert_eq!(cost(gate_named("osi_wifi_delete_queue")), 13_178, "event posting adds no work to queue deletion");
    let largest = symbols.gate_reports.iter().max_by_key(|g| cost(g)).unwrap();
    assert_eq!((largest.name.as_str(), cost(largest)), ("osi_task_create", 15_801));
}

#[test]
fn slot_carries_island_header_thunks_and_segments() {
    let (radio, slot, _) = planned_slot();
    let island = &radio.island;
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let tables = symbols.island_tables;
    let word = |addr: u32| {
        let at = (addr - SLOT_DATA_BASE) as usize;
        u32::from_le_bytes(slot.bytes[at..at + 4].try_into().unwrap())
    };
    let header = island.header.encode();
    let at = (tables.header - SLOT_DATA_BASE) as usize;
    assert_eq!(&slot.bytes[at..at + header.len()], &header[..], "all header words in wire order");

    let (mut memory, _) = slot_memory(slot, radio.heap_bytes);
    let blob = symbols.blobs.iter().find(|b| b.name == "radio_island_thunks").unwrap();
    assert_eq!(symbols.island_thunks.len(), island.gate_names.len());
    for (index, name) in island.gate_names.iter().enumerate() {
        let thunk = word(tables.gates + 4 * index as u32);
        assert_eq!((&symbols.island_thunks[index].0, symbols.island_thunks[index].1), (name, thunk));
        assert!((blob.start..blob.end).contains(&thunk), "gate {name} has a windowed island thunk");
        let gate = symbols.gate_reports.iter().find(|g| &g.name == name).unwrap();
        let mut cpu = Cpu::new(s3_config(), thunk);
        cpu.ps = radio::WINDOWED_PS;
        cpu.ar[1] = STACK_TOP;
        // Enter with CALLINC=2 as after CALLX8. Each untraced thunk calls its gate before it can return or block.
        for _ in 0..blob.end - blob.start {
            if !(blob.start..blob.end).contains(&cpu.pc) { break; }
            cpu.step(&mut memory).unwrap();
        }
        assert_eq!(cpu.pc, gate.address, "island gate {index} calls the kernel export {name}");
    }

    let mut addr = tables.segments;
    for s in &island.segments {
        let dst = match s.kind {
            SegmentKind::Iram => SRAM_START + (s.address - IRAM_START),
            SegmentKind::Data | SegmentKind::Bss => s.address,
            SegmentKind::Text | SegmentKind::Rodata => continue,
        };
        if s.kind != SegmentKind::Bss {
            assert_eq!(word(addr), s.file_offset, "{} source offset", s.kind.name());
            addr += 4;
        }
        assert_eq!((word(addr), word(addr + 4)), (dst, s.bytes), "{} exact destination and bytes", s.kind.name());
        addr += 8;
    }
}

fn boot_image(island: &Island, source: &str, task: BootTask) -> Image {
    let radio = Radio { task_capacity: 3, boot_tasks: vec![task], ..profile(island.binding()) };
    let app = app(source);
    let module = module::load(&app.module, &chip::profile()).unwrap();
    build_image(&module, &board(radio), &SlotOptions::default()).unwrap()
}

fn ram_word(machine: &S3Machine, at: u32) -> u32 {
    let mut bytes = [0; 4];
    assert!(machine.read_data(at, &mut bytes), "read SRAM at {at:#010x}");
    u32::from_le_bytes(bytes)
}

fn insn(bytes: &[u8]) -> Option<Insn> {
    let bits = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]);
    decode(bits).ok().map(|(i, _)| i)
}

fn store_width(op: Op) -> Option<u32> {
    match op {
        Op::S8i => Some(1), Op::S16i => Some(2),
        Op::S32i | Op::S32iN | Op::S32e | Op::Ssi | Op::Ssip | Op::Ssx | Op::Ssxp => Some(4),
        _ => None,
    }
}

fn constant_into(i: &Insn, reg: u8) -> bool {
    match i.op {
        Op::MoviN => i.s == reg,
        Op::Movi | Op::L32r => i.t == reg,
        _ => false,
    }
}

fn scalar_store(i: Insn) -> bool {
    matches!(i.op, Op::S8i | Op::S16i | Op::S32i | Op::S32iN)
}

// Resolve the actual base from the instruction immediately before the store.
fn inline_store_address(slot: &Slot, pc: u32, i: Insn, before: &[Insn]) -> Option<u32> {
    let load = before.last()?;
    if !scalar_store(i) || !constant_into(load, i.s) { return None; }
    let base = match load.op {
        Op::Movi | Op::MoviN => load.imm as u32,
        Op::L32r => {
            let literal = fab_elec_mcu_xtensa::insn::target(load, pc - 3)?;
            let at = literal.checked_sub(SLOT_BASE)? as usize;
            u32::from_le_bytes(slot.bytes.get(at..at + 4)?.try_into().ok()?)
        }
        _ => return None,
    };
    base.checked_add(i.imm as u32)
}

fn region_store_address(before: &[Insn], src: u8) -> bool {
    if src == 8 { return false; }
    // Strip only the source reload or rematerialization between address() and the store.
    let address = match before {
        [address @ .., offset, add, load] if constant_into(offset, src)
            && (add.op, add.r, add.s, add.t) == (Op::AddN, src, 1, src)
            && (load.op, load.t, load.s, load.imm) == (Op::L32i, src, src, 0) => address,
        [address @ .., load] if (load.op, load.t, load.s) == (Op::L32i, src, 1)
            || constant_into(load, src) => address,
        _ => before,
    };
    matches!(address, [.., Insn { op: Op::AddN, r: 8, s: 8, t: 1 | 12 | 13, .. }])
        || matches!(address, [.., Insn { op: Op::AddN, r: 11, s: 11, t: 1 | 12 | 13, .. },
            Insn { op: Op::Addx4, r: 8, t: 11, .. }])
}

fn export_store(i: Insn, before: &[Insn], in_prologue: bool) -> Option<&'static str> {
    if i.op != Op::S32i { return None; }
    if i.s == 1 { return Some("frame saves and spills"); }
    if i.imm != 0 { return None; }
    if matches!(i.s, 8 | 11) && i.t != i.s && matches!(before, [.., offset, add]
        if constant_into(offset, i.s) && (add.op, add.r, add.s, add.t) == (Op::AddN, i.s, 1, i.s)) {
        return Some("frame saves and spills");
    }
    if in_prologue && (i.t, i.s) == (2, 3) && matches!(before, [..,
        Insn { op: Op::MoviN, s: 2, imm: 0, .. }, Insn { op: Op::Addi, t: 3, s: 1, imm: 16 | 32, .. },
        words, Insn { op: Op::Beqz, s: 4, .. }] if constant_into(words, 4)) {
        return Some("frame clear");
    }
    if i.s == 8 && region_store_address(before, i.t) { return Some("region stores"); }
    None
}

// A fresh history per symbol keeps address forms within the same routine. None is
// one of the two store opcodes omitted by the model, never an address-forming instruction.
fn scan_stores(slot: &Slot, name: &str, start: u32, end: u32,
    counts: &mut std::collections::BTreeMap<&'static str, usize>,
    classify: impl Fn(u32, Option<Insn>, &[Insn]) -> Option<&'static str>) -> usize {
    assert!(start < end, "empty code span for {name}");
    let mut bytes = &slot.bytes[(start - SLOT_BASE) as usize..(end - SLOT_BASE) as usize];
    let mut before = Vec::new();
    let mut targets = Vec::new();
    let mut proven = Vec::new();
    let mut pc = start;
    let mut stores = 0;
    while !bytes.is_empty() {
        let raw = &bytes[..bytes.len().min(3)];
        let mut word = [0; 4];
        word[..raw.len()].copy_from_slice(raw);
        let bits = u32::from_le_bytes(word);
        // Xtensa ISA Table 233 (LSAI), r=14 S32C1I and r=15 S32RI.
        let (instruction, len) = if matches!(bits & 0xf00f, 0xe002 | 0xf002) {
            (None, 3)
        } else {
            let (i, len) = decode(bits).unwrap_or_else(|_| panic!("undecoded instruction in {name} at {pc:#010x}: {raw:02x?}"));
            (Some(i), len as usize)
        };
        assert!(len <= bytes.len(), "instruction crosses end of {name} at {pc:#010x}: {raw:02x?}");
        if instruction.is_none_or(|i| store_width(i.op).is_some()) {
            let class = classify(pc, instruction, &before).unwrap_or_else(||
                panic!("unknown store in {name} at {pc:#010x}: {:02x?}", &bytes[..len]));
            if class == "proven stores" { proven.push(pc); }
            *counts.entry(class).or_default() += 1;
            stores += 1;
            before.clear();
        } else if let Some(i) = instruction {
            if i.op != Op::L32r {
                if let Some(target) = fab_elec_mcu_xtensa::insn::target(&i, pc) { targets.push(target); }
            }
            before.push(i);
        }
        pc += len as u32;
        bytes = &bytes[len..];
    }
    assert_eq!(pc, end, "whole instructions tile {name}");
    for target in targets {
        assert!(proven.binary_search(&target).is_err(), "{name}: branch to {target:#010x} skips the proved address load");
    }
    stores
}

#[test]
fn planned_kernel_stores_use_checked_ports_or_emitter_addresses() {
    let (_, slot, _) = planned_slot();
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let blob = |name| symbols.blobs.iter().find(|b| b.name == name).unwrap();
    let objects = blob("radio_objects");
    let gates = blob("radio_gates");
    let mut counts = std::collections::BTreeMap::new();
    let mut previous_end = objects.start;
    for (name, start, size) in &symbols.kernel_symbols {
        let end = start.checked_add(*size).unwrap();
        assert!(previous_end <= *start && end <= objects.end, "{name} routine span");
        scan_stores(slot, name, *start, end, &mut counts, |_, i, _| {
            if !scalar_store(i?) { return None; }
            match name.as_str() {
                "kernel_fault" => Some("fault stub"),
                "store8" | "store16" | "store32" | "pack_store8" | "pack_store32" | "dev_store32"
                    | "copy_raw" | "pack_copy" | "fill_raw" | "pack_fill" | "now64" | "report" | "radio_stop" => Some("port routines"),
                _ => None,
            }
        });
        previous_end = end;
    }

    let reports = &symbols.gate_reports;
    let map = &symbols.kernel_map;
    assert_eq!(map.entries.len(), reports.len(), "one gate per export");
    let mut emitter_stores = 0;
    for (index, (&(start, end), gate)) in map.entries.iter().zip(reports).enumerate() {
        assert!(previous_end <= start && end <= objects.end, "{} export span", gate.name);
        let first = map.ops.partition_point(|op| op.entry < index);
        let prologue_end = map.ops.get(first).filter(|op| op.entry == index).map_or(end, |op| op.start);
        let sites = &gate.proven_accesses;
        assert!(sites.windows(2).all(|s| s[0].pc < s[1].pc), "{}: unique ordered proven PCs", gate.name);
        for site in sites {
            assert!((start..end).contains(&site.pc), "{}: proven instruction belongs to its export", gate.name);
            assert!(symbols.kernel_regions.iter().any(|(r, writable, kind)| site.class.reaches(*kind)
                && (!site.write || *writable) && site.address % site.width == 0 && r.start <= site.address
                && u64::from(site.address) + u64::from(site.width) <= u64::from(r.end)),
                "{}: proven access fits its class and permissions: {site:?}", gate.name);
        }
        let previous = counts.get("proven stores").copied().unwrap_or(0);
        emitter_stores += scan_stores(slot, &gate.name, start, end, &mut counts, |pc, i, before| {
            let i = i?;
            if let Ok(n) = sites.binary_search_by_key(&pc, |s| s.pc) {
                let site = &sites[n];
                return (site.write && store_width(i.op) == Some(site.width)
                    && inline_store_address(slot, pc, i, before) == Some(site.address)).then_some("proven stores");
            }
            export_store(i, before, pc < prologue_end)
        });
        assert_eq!(counts.get("proven stores").copied().unwrap_or(0) - previous,
            sites.iter().filter(|s| s.write).count(), "{}: every recorded store PC decodes to its proved access", gate.name);
        previous_end = end;
    }
    let proven = counts.get("proven stores").copied().unwrap_or(0);
    assert!(emitter_stores > proven, "the scan classified emitter frame and state stores in export text");
    assert!(proven > 0, "the production kernel stores through proved constant addresses");

    // Gate starts follow the literal pool. Only the last gate is followed by
    // radio_blob's word alignment, with zero to three zero bytes after RET.N.
    let last = reports.last().unwrap().address;
    let tail = &slot.bytes[(last - SLOT_BASE) as usize..(gates.end - SLOT_BASE) as usize];
    let last_len = tail.iter().rposition(|&byte| byte != 0).unwrap() + 1;
    let end = last + last_len as u32;
    assert!(tail[..last_len].ends_with(&[0x0d, 0xf0]) && end.next_multiple_of(4) == gates.end,
        "the final gate ends in RET.N followed only by alignment padding");
    for (index, gate) in reports.iter().enumerate() {
        let next = reports.get(index + 1).map_or(end, |g| g.address);
        assert!(gates.start <= gate.address && next <= end, "{} gate span", gate.name);
        scan_stores(slot, &gate.name, gate.address, next, &mut counts, |_, i, before| {
            let i = i?;
            if i.op != Op::S32i { return None; }
            match i.s {
                1 => Some("gate stack"),
                // The S3 has no CONST16 encoding (XCHAL_HAVE_CONST16 is zero).
                8 if i.imm == 0 && matches!(before.last(), Some(Insn { op: Op::L32r | Op::Movi, t: 8, .. })) =>
                    Some("gate argument slots"),
                _ => None,
            }
        });
    }
    eprintln!("| Store class | Instructions |\n| --- | ---: |");
    for class in ["port routines", "fault stub", "frame saves and spills", "frame clear", "region stores",
        "proven stores", "gate stack", "gate argument slots"] {
        eprintln!("| {class} | {} |", counts.get(class).copied().unwrap_or(0));
    }
}

// Stop before stores, including stores that would leave the old byte value in place.
// Scan all byte alignments so literal pools and variable-width instructions need no special handling.
struct StoreWatch<'a> {
    image: &'a Image,
    stops: Vec<u32>,
    marks: PcSet,
    forbidden: Vec<std::ops::Range<u32>>,
}

impl<'a> StoreWatch<'a> {
    fn new(image: &'a Image, extra: &[u32], forbidden: Vec<std::ops::Range<u32>>) -> Self {
        let mut stops = extra.to_vec();
        for (base, bytes) in [(s3_fixture::FLASH, &image.flash[..VOICEBOX.records[0] as usize]),
            (SLOT_BASE, image.slot.bytes.as_slice())] {
            for (at, bytes) in bytes.windows(3).enumerate() {
                if insn(bytes).and_then(|i| store_width(i.op)).is_some() { stops.push(base + at as u32); }
            }
        }
        for range in &forbidden { stops.extend(range.clone()); }
        stops.sort_unstable(); stops.dedup();
        let first = stops[0];
        let mut marks = PcSet::new(first, stops.last().unwrap() - first + 1);
        for &pc in &stops { marks.insert(pc); }
        Self { image, stops, marks, forbidden }
    }

    fn advance(&self, m: &mut S3Machine, deadline: u64, mut stop: impl FnMut(&S3Machine, Option<(u32, u32)>) -> bool) -> Ran {
        loop {
            match m.run(&Until { tick: deadline, marks: &self.marks, stops: &self.stops,
                stops_after: m.now(), on_drive: false }) {
                Ran::Pc => {
                    let pc = m.pc();
                    assert!(self.forbidden.iter().all(|r| !r.contains(&pc)), "failed binding attempted execution at {pc:#010x}");
                    let bytes = if pc >= SLOT_BASE { &self.image.slot.bytes[(pc - SLOT_BASE) as usize..] }
                        else { &self.image.flash[(pc - s3_fixture::FLASH) as usize..] };
                    let i = insn(bytes).expect("executing instruction decodes");
                    let store = store_width(i.op).map(|width| {
                        let ar = &m.cpu().ar;
                        let offset = match i.op {
                            Op::Ssip | Op::Ssxp => 0, Op::Ssx => ar[i.t as usize], _ => i.imm as u32,
                        };
                        (ar[i.s as usize].wrapping_add(offset), width)
                    });
                    if stop(m, store) { return Ran::Pc; }
                }
                Ran::Tick => return Ran::Tick,
                Ran::Halted if matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break) => return Ran::Halted,
                ran => panic!("island scenario stopped at {ran:?}: {:?}", m.status()),
            }
        }
    }
}

fn reach(m: &mut S3Machine, pc: u32, deadline: u64) {
    let mut marks = PcSet::new(pc, 1); marks.insert(pc);
    assert_eq!(m.run(&Until { tick: deadline, marks: &marks, stops: &[pc], stops_after: m.now(), on_drive: false }),
        Ran::Pc, "reach {pc:#010x}: {:?}", m.status());
}

// The slot CRC is the first call after binding in these pinless apps, before the dispatcher drains reports.
fn boot_past_island_start(image: &Image, flash_island: &[u8]) -> S3Machine {
    let mut m = island_fixture::machine_with_island(&image.flash, flash_island, &s3_fixture::profile(53));
    let crc = image.slot.map.runtime.iter().find(|r| r.name == "image_crc32").unwrap();
    let deadline = m.tick_hz();
    reach(&mut m, crc.start, deadline);
    m
}

// ESP-IDF v6.1.1 ESP32-S3 rtc_cntl_reg.h:1774,1872 and syscon_reg.h:145–155.
const WIFI_POWER_REGS: [u32; 4] = [0x6000_8090, 0x6000_8094, 0x6002_6014, 0x6002_6018];

fn rebuild_ranges(slot: &Slot, window_end: u32) -> Vec<std::ops::Range<u32>> {
    use fab_elec_mcu_emit_s3::RadioSram;
    slot.layout.radio.as_ref().unwrap().clear_spans().into_iter().chain([
        SRAM_START..window_end,
        RadioSram::PHY_ROM_RAM..RadioSram::PHY_ROM_RAM + 4 * RadioSram::PHY_ROM_WORDS,
    ]).collect()
}

fn ram_bytes(m: &S3Machine, range: &std::ops::Range<u32>) -> Vec<u8> {
    let mut bytes = vec![0; (range.end - range.start) as usize];
    assert!(m.read_data(range.start, &mut bytes), "snapshot must cover SRAM at {range:#x?}");
    bytes
}

fn rebuild_registers() -> [u32; 8] {
    use radio::kernel::INTR_MATRIX;
    let [pwc, iso, clk, rst] = WIFI_POWER_REGS;
    [pwc, iso, clk, rst, INTR_MATRIX, INTR_MATRIX + 4, INTR_MATRIX + 8, INTR_MATRIX + 12]
}

#[test]
fn requested_restart_rebuilds_boot_state_and_orders_start_markers() {
    exercise_rebuilds(3, false);
}

#[test]
fn restart_repeats_without_drift() {
    exercise_rebuilds(6, true);
}

fn exercise_rebuilds(last_generation: u32, check_drift: bool) {
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, enc, reg::*, Asm};
    use fab_elec_mcu_emit_s3::RadioSram;
    use radio::{kernel as k, BLOCKED, READY, TCB_PRIORITY, TCB_STATE, YIELD_LINE};

    let island = island_fixture::linked_island(vec![1, 3, 5], 64);
    // In P5 the first BSS word holds the live PHY table pointer.
    let ets = island.segments.iter().find(|s| s.kind == SegmentKind::Bss).unwrap().address + 4 * u32::from(check_drift);
    let phy_at = RadioSram::ROM_DATA_END - 4 * RadioSram::PHY_TABLE_WORDS;
    let data = STACK_TOP - 0x4000;
    let log = data - 64;
    let entry = IRAM_START + (data - SRAM_START);
    let radio = Radio { dispatcher_priority: 2, task_capacity: 3, irq_stack_bytes: 2048,
        boot_tasks: vec![BootTask { entry, abi: radio::TaskAbi::Windowed, priority: 3,
            stack_bytes: 2048, args: [0; 6] }], ..profile(island.binding()) };
    let app = app("var ticks: i32 = 0\nlet tick = every(ms(2), fn() { ticks += 1 })");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let board = board(radio);
    let image = build_image(&module, &board, &SlotOptions::default()).unwrap();
    let slot = &image.slot; let r = slot.layout.radio.as_ref().unwrap(); let s = slot.symbols.radio.as_ref().unwrap();
    let crit_max_span = check_drift.then(|| {
        let compiled = fixture_kernel(board.radio.as_ref().unwrap(), slot);
        let kernel = module::load_kernel(&compiled.module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
        let var = kernel.vars().iter().find(|v| kernel.string(v.name) == "crit_max_span")
            .expect("P5 locates the critical-section high-water mark by name");
        r.kernel_state + 4 * var.offset
    });
    let ticks = slot.layout.state + 4 * module.vars().iter().find(|v| module.string(v.name) == "ticks").unwrap().offset;
    let registers = rebuild_registers();
    let thunk = |name: &str| s.thunks.iter().find(|(n, _)| n == name).unwrap().1;
    let call = |a: &mut Asm, target: u32, args: &[u32]| {
        for (i, &arg) in args.iter().enumerate() { a.mov_reg_i32(A10 + i as u8, arg); }
        a.mov_reg_i32(A8, target); a.callx8(A8);
    };
    let write = |a: &mut Asm, addr, value| {
        a.mov_reg_i32(A3, addr); a.mov_reg_i32(A4, value); a.s32i(A4, A3, 0);
    };
    let mut sampled = 0; let mut requesting = 0; let mut ran_root = 0;
    let worker = assemble(&mut Asm::new(3, A8), |a| {
        const WAIT: usize = 0; const TICKED: usize = 1; const PARK: usize = 2;
        a.literal_table(); a.entry(A1, 64); a.mov_reg_i32(A5, log);
        for (i, address) in registers.into_iter().enumerate() {
            a.mov_reg_i32(A3, address); a.l32i(A4, A3, 0); a.s32i(A4, A5, 4 * i as u32);
        }
        sampled = entry + a.base.offset();
        call(a, island.entries[1], &[]);
        ran_root = entry + a.base.offset();
        a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0);
        if !check_drift { a.bcci_reg_label(cc::EQ, A4, last_generation as i32, PARK); }
        call(a, thunk("osi_set_intr"), &[0, 0, 3, 1]);
        call(a, thunk("osi_set_isr"), &[3, island.entries[0], 0]);
        call(a, thunk("osi_ints_on"), &[1 << 3]);
        call(a, thunk("queue_create"), &[2, 4]); a.mov_reg_i32(A3, log); a.s32i(A10, A3, 32);
        call(a, thunk("timer_setfn"), &[ets, island.entries[0], 42]);
        call(a, thunk("timer_arm_us"), &[ets, 1_000_000, 0]); a.mov_reg_i32(A3, log); a.s32i(A10, A3, 36);
        call(a, thunk("osi_wifi_int_disable"), &[0]);
        a.mov_n(A11, A10); a.mov_reg_i32(A3, s.crit_depth); a.l32i(A4, A3, 0);
        a.mov_reg_i32(A3, log); a.s32i(A4, A3, 40);
        call(a, thunk("osi_wifi_int_restore"), &[0]);
        write(a, RadioSram::PHY_ROM_RAM, 0xdec0_ded1);
        write(a, RadioSram::PHY_ROM_RAM + 312, 0xdec0_ded2);
        if check_drift {
            write(a, island.objects[radio::PHY_FUNS_OBJECT].start, phy_at);
            write(a, s.phy_calibrated, 1);
        }
        for address in [k::SYSCON_WIFI_CLK_EN, k::SYSCON_WIFI_RST_EN] {
            a.mov_reg_i32(A3, address); a.l32i(A4, A3, 0); a.mov_reg_i32(A5, 1 << 29);
            a.or(A4, A4, A5); a.s32i(A4, A3, 0);
        }
        a.mov_reg_i32(A3, k::INTR_MATRIX); a.l32i(A4, A3, 0); a.mov_reg_i32(A3, log); a.s32i(A4, A3, 44);
        // Let the dispatcher drain the boot marker and actually service an app timer.
        a.mov_reg_i32(A6, ticks); a.l32i(A7, A6, 0);
        write(a, r.boot_tasks[0] + TCB_PRIORITY, 1);
        a.movi(A4, 1 << YIELD_LINE); a.wsr(A4, 226); a.rsync();
        a.base.label_assign(WAIT); a.l32i(A4, A6, 0); a.bcc_reg_reg_label(cc::NE, A4, A7, TICKED);
        a.waiti(0); a.j_label(WAIT);
        a.base.label_assign(TICKED); a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        for i in 0..radio::REPORT_RING_ENTRIES {
            a.movi(A2, 96); a.movi(A3, i as i32); a.movi(A10, 123);
            a.mov_reg_i32(A4, s.report); a.callx0(A4);
        }
        requesting = entry + a.base.offset();
        if check_drift {
            a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0);
            a.bcci_reg_label(cc::EQ, A4, last_generation as i32, PARK);
        }
        write(a, r.request, 1);
        a.base.label_assign(PARK); a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        write(a, r.boot_tasks[0] + TCB_STATE, BLOCKED); write(a, r.dispatcher + TCB_STATE, READY);
        a.movi(A4, 1 << YIELD_LINE); a.wsr(A4, 226); a.rsync();
        a.op24(enc::rrr(0, 0, 0, 6, 0, A3)); a.j(-4);
    });
    assert!(slot.layout.regions[0].used <= u64::from(log) && data + worker.len() as u32 <= STACK_TOP - slot.layout.stack_bytes);
    let mut p = s3_fixture::profile(64);
    let stage = p.entry;
    let setup = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table(); a.movi(A0, 15); a.wsr(A0, 230); a.rsync();
        write(a, k::SYSCON_WIFI_CLK_EN, 0x8100_0041);
        write(a, k::SYSCON_WIFI_RST_EN, 0x4000_8000);
        a.mov_reg_i32(A3, stage); a.jx(A3);
    });
    p.entry = IRAM_START + (slot.layout.t0 - SRAM_START);
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &p);
    let rom: Vec<_> = (0..316).map(|i| (7 + 13 * i) as u8).collect();
    assert!(m.seed_data(RadioSram::PHY_ROM_RAM, &rom) && m.seed_data(slot.layout.t0, &setup) && m.seed_data(data, &worker));
    let phy: Vec<_> = (0..RadioSram::PHY_TABLE_WORDS).flat_map(|i| (0xabc0_0000 | i).to_le_bytes()).collect();
    if check_drift {
        assert!(m.seed_data(phy_at, &phy), "P5 seeds a distinct PHY function in every slot");
        assert!(m.seed_data(slot.layout.stack_top - slot.layout.stack_bytes, &vec![0; slot.layout.stack_bytes as usize]),
            "P5 gives the dispatcher stack the same zero fill as the radio stacks");
    }
    let deadline = m.tick_hz();
    let spans = r.clear_spans();
    assert_eq!(spans, [r.start..r.stop_cause, r.stop_cause + 4..r.reports,
        r.next_start + 8..r.end]);
    assert_eq!([r.started_at, r.next_start], [r.phy_table + 656, r.phy_table + 664],
        "P1 keeps both restart timestamps in the survivor block");
    assert_eq!(r.generation, r.reports + 12 + 12 * radio::REPORT_RING_ENTRIES);
    assert_eq!([r.generation, r.consecutive, r.request, r.restart_return, r.boot_wifi_clk,
        r.boot_wifi_rst, r.boot_phy_ram], std::array::from_fn(|i| r.generation + 4 * i as u32));
    assert_eq!([r.phy_table_state, r.phy_table_at, r.phy_table],
        [r.boot_phy_ram + 316, r.boot_phy_ram + 320, r.boot_phy_ram + 324]);
    let ranges = rebuild_ranges(slot, island.window_end);
    let snapshot = |m: &S3Machine| ranges.iter().map(|range| ram_bytes(m, range)).collect::<Vec<_>>();
    let island_lines = !(runtime::DISPATCHER_LINES | (1 << YIELD_LINE));
    let watch = StoreWatch::new(&image, &[s.restore, s.started], vec![]);
    reach(&mut m, s.started, deadline);
    let boot = snapshot(&m); assert_eq!(boot.last().unwrap(), &rom);
    let boot_lines = m.cpu().intenable & island_lines;
    let check_start = |m: &S3Machine, generation: u32| {
        assert_eq!(m.pc(), s.started);
        let heap = r.heap..r.heap + board.radio.as_ref().unwrap().heap_bytes;
        assert!(ram_bytes(m, &heap).iter().all(|&b| b == 0),
            "rebuild generation {generation} must supply the zeroed heap required by objects.splash before the first allocation");
        assert_eq!(ram_word(m, r.island_status), radio::ISLAND_READY);
        assert_eq!((ram_word(m, r.generation), ram_word(m, r.consecutive)), (generation, generation - 1));
        assert_eq!(ram_word(m, r.stop_cause), u32::from(generation > 1));
        assert_eq!(m.cpu().ps & 15, 15); assert_eq!(m.cpu().intenable & island_lines, boot_lines);
        if check_drift {
            assert_eq!([r.boot_wifi_clk, r.boot_wifi_rst].map(|at| ram_word(m, at)), [0x8100_0041, 0x4000_8000],
                "P5 generation {generation} retains both pre-boot clock and reset snapshots");
            assert_eq!(ram_bytes(m, &(r.boot_phy_ram..r.phy_table_state)), rom, "P5 generation {generation} keeps the boot ROM snapshot");
            assert_eq!(ram_bytes(m, &(phy_at..RadioSram::ROM_DATA_END)), phy, "P5 generation {generation} restores the PHY functions");
            assert_eq!(ram_word(m, r.phy_table_state), u32::from(generation > 1), "P5 saves the table after first calibration");
            if generation > 1 {
                assert_eq!(ram_word(m, r.phy_table_at), phy_at, "P5 generation {generation} retains the saved pointer");
                assert_eq!(ram_bytes(m, &(r.phy_table..r.started_at)), phy, "P5 generation {generation} keeps the saved table");
            }
        }
        for ((range, expected), got) in ranges.iter().zip(&boot).zip(snapshot(m)) {
            if let Some(i) = expected.iter().zip(&got).position(|(a, b)| a != b) {
                panic!("generation {generation}, {:#010x}: boot {:#04x}, rebuilt {:#04x}", range.start + i as u32, expected[i], got[i]);
            }
        }
    };
    check_start(&m, 1);
    let mut boot_regs = None; let mut previous_ticks = 0;
    let mut first_resources = None;
    let mut previous_cursors = None;
    for generation in 1..=last_generation {
        // MMIO has no debug read. Verify no intervening slot store, then read all eight
        // words with the restored worker's first instructions, before any gate or MMIO write.
        assert_eq!(watch.advance(&mut m, deadline, |m, store| {
            assert!(!store.is_some_and(|(addr, _)| registers.contains(&addr)));
            m.pc() == s.restore
        }), Ran::Pc);
        assert_eq!(m.cpu().ar[2], r.boot_tasks[0]);
        reach(&mut m, sampled, deadline);
        let regs: [u32; 8] = std::array::from_fn(|i| ram_word(&m, log + 4 * i as u32));
        assert_eq!(regs, *boot_regs.get_or_insert(regs));
        reach(&mut m, ran_root, deadline); assert_eq!(m.cpu().ar[10], 1, "island entry returned in generation {generation}");
        if generation == last_generation && !check_drift { break; }
        reach(&mut m, requesting, deadline);
        assert_eq!(ram_word(&m, r.isr_count), 1); assert_eq!(ram_word(&m, log + 44), 3);
        assert_ne!(m.cpu().intenable & (1 << 3), 0);
        assert_ne!(ram_word(&m, log + 32), 0, "queue allocated");
        assert_eq!(ram_word(&m, log + 36), 1, "timer armed");
        assert_eq!((ram_word(&m, log + 40), ram_word(&m, s.crit_depth)), (1, 0));
        assert_eq!(ram_word(&m, RadioSram::PHY_ROM_RAM + 312), 0xdec0_ded2);
        assert!(ram_word(&m, ticks) > previous_ticks); previous_ticks = ram_word(&m, ticks);
        assert_eq!(ram_word(&m, r.reports + 4).wrapping_sub(ram_word(&m, r.reports)), radio::REPORT_RING_ENTRIES);
        assert_eq!(ram_word(&m, r.reports + 8), 0, "every old report fit");
        if check_drift {
            let head = ram_word(&m, r.reports); let tail = ram_word(&m, r.reports + 4);
            if let Some((h, t)) = previous_cursors.replace((head, tail)) {
                assert_eq!((head.wrapping_sub(h), tail.wrapping_sub(t)), (9, 9),
                    "P5 generation {generation}: one start marker drained plus eight reports appended between checkpoints");
            }
            // Heap free bytes, free blocks, armed/live/high-water timers, reserved slots,
            // and Q_COUNT in the allocated queue (objects.splash).
            let q = ram_word(&m, log + 32);
            let mut resources: Vec<_> = [r.heap + 8, r.heap + 16, r.heap + 36, r.heap + 40,
                r.heap + 48, r.heap + 52, q + 16, r.isr_max_span, crit_max_span.unwrap()].into_iter()
                .map(|at| (at, ram_word(&m, at))).collect();
            assert_eq!(resources[2].1, 1, "P5 generation {generation} measures a live armed timer");
            let dispatcher = slot.layout.stack_top - slot.layout.stack_bytes..slot.layout.stack_top;
            for stack in std::iter::once(dispatcher).chain([&r.idle_stack, &r.irq_stack].into_iter()
                .chain(&r.boot_stacks).map(|s| s.base..s.top)) {
                assert_eq!(ram_word(&m, stack.start), radio::GUARD, "P5 generation {generation} stack guard at {:#x}", stack.start);
                let used = ram_bytes(&m, &(stack.start + 4..stack.end));
                let deepest = used.chunks_exact(4).position(|word| word != [0; 4])
                    .map_or(stack.end, |n| stack.start + 4 + 4 * n as u32);
                resources.push((stack.start, deepest));
            }
            assert_eq!(ram_word(&m, r.island_guard), radio::GUARD, "P5 generation {generation} retains the island guard");
            assert_eq!(resources, *first_resources.get_or_insert_with(|| resources.clone()),
                "P5 generation {generation} must match generation 1 at each resource address");
            if generation == last_generation { break; }
        }
        reach(&mut m, s.restart, deadline);
        assert_eq!(ram_word(&m, r.request), 1);
        let (ret, begin, tick) = (m.cpu().ar[0], m.cpu().instret, m.now());
        let (sp, ps) = (m.cpu().ar[1], m.cpu().ps);
        let saved: [u32; 4] = m.cpu().ar[12..16].try_into().unwrap();
        let mut power = Vec::new();
        assert_eq!(watch.advance(&mut m, deadline, |m, store| {
            if let Some((addr, width)) = store.filter(|(a, _)| WIFI_POWER_REGS.contains(a)) {
                assert_eq!(width, 4);
                let i = insn(&slot.bytes[(m.pc() - SLOT_BASE) as usize..]).unwrap();
                power.push((addr, m.cpu().ar[i.t as usize]));
            }
            m.pc() == s.started
        }), Ran::Pc);
        check_start(&m, generation + 1);
        let [pwc, iso, clk, rst] = WIFI_POWER_REGS;
        let boot_clk = ram_word(&m, r.boot_wifi_clk); let boot_rst = ram_word(&m, r.boot_wifi_rst);
        assert_eq!(power, [(rst, regs[3] | (1 << 29) | k::MODEM_RESET_FIELD),
            (iso, regs[1] | k::WIFI_FORCE_ISO), (pwc, regs[0] | k::WIFI_FORCE_PD),
            (clk, boot_clk), (rst, boot_rst | k::MODEM_RESET_FIELD),
            (pwc, regs[0]), (clk, boot_clk | k::CLK_COMMON), (rst, boot_rst | k::MODEM_RESET_FIELD),
            (rst, regs[3]), (iso, regs[1]), (clk, regs[2])], "quiesce, isolate, power down, restore and power on");
        reach(&mut m, ret, deadline);
        assert_eq!((m.cpu().ar[1], m.cpu().ps, &m.cpu().ar[12..16]), (sp, ps, &saved[..]));
        assert_eq!(ram_word(&m, r.request), 0);
        let retired = m.cpu().instret - begin;
        assert!(retired <= s.restart_max_insns, "{retired} retired, bound {}", s.restart_max_insns);
        let elapsed = m.now() - tick;
        assert!(elapsed <= m.tick_hz() / 20, "{elapsed} ticks masked exceeds 50 ms");
        eprintln!("restart retired {retired}, bound {}, {elapsed} ticks, clear spans {spans:#x?}", s.restart_max_insns);
    }
    let until = m.now() + m.tick_hz() / 50;
    assert_eq!(m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }), Ran::Tick);
    assert!(ram_word(&m, ticks) > previous_ticks && m.last_reset().is_none());
    let mut out = Vec::new(); m.take_out(&mut out); m.mark();
    let mut reader = watch::Reader::new(); let mut events = Vec::new();
    for byte in out {
        if let Some(frame) = reader.push(byte.byte) {
            match frame.unwrap().event {
                watch::Event::Fault { code, site, arg } => events.push((code, site, arg)),
                watch::Event::Lost { .. } => panic!("lost reports across a restart"),
                _ => {}
            }
        }
    }
    let mut expected = vec![(watch::FAULT_RADIO_STARTED, watch::SITE_RADIO_SCHEDULER, 1)];
    for generation in 2..=last_generation as i32 {
        expected.extend((0..radio::REPORT_RING_ENTRIES).map(|i| (96, 123, i as i32)));
        expected.push((watch::FAULT_RADIO_STARTED, watch::SITE_RADIO_SCHEDULER, generation | (1 << 16) | ((generation - 1) << 24)));
    }
    if check_drift { expected.extend((0..radio::REPORT_RING_ENTRIES).map(|i| (96, 123, i as i32))); }
    assert_eq!(events, expected, "each generation follows every report of its predecessor");
}

#[test]
fn restart_restores_the_phy_table_and_refuses_one_outside_rom_ram() {
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, enc, reg::*, Asm};
    use fab_elec_mcu_emit_s3::RadioSram;

    const WORDS: u32 = 164;
    const LAST: u32 = 0x3fce_fd70;
    const FIRST_VALUE: u32 = 0x7123_4000;
    let island = island_fixture::small_island();
    let object = island.objects[radio::PHY_FUNS_OBJECT].start;
    let data = STACK_TOP - 0x4000;
    let entry = IRAM_START + (data - SRAM_START);
    let radio = Radio { dispatcher_priority: 2, task_capacity: 3,
        boot_tasks: vec![BootTask { entry, abi: radio::TaskAbi::Windowed, priority: 3,
            stack_bytes: 1024, args: [0; 6] }], ..profile(island.binding()) };
    let app = app("let tick = every(ms(2), fn() {})");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let image = build_image(&module, &board(radio), &SlotOptions::default()).unwrap();
    let r = image.slot.layout.radio.as_ref().unwrap();
    let s = image.slot.symbols.radio.as_ref().unwrap();
    let words = |m: &S3Machine, at, count: u32| (0..count).map(|i| ram_word(m, at + 4 * i)).collect::<Vec<_>>();
    let restart_stores = StoreWatch::new(&image, &[s.started], vec![]);
    let save_pc = image.slot.map.runtime.iter().find(|routine| routine.name == "radio_phy_table").unwrap().start;
    let write = |a: &mut Asm, at, value| {
        a.mov_reg_i32(A3, at); a.mov_reg_i32(A4, value); a.s32i(A4, A3, 0);
    };
    let dispatch = |a: &mut Asm| {
        write(a, r.dispatcher + radio::TCB_STATE, radio::READY);
        a.movi(A4, 1 << radio::YIELD_LINE); a.wsr(A4, 226);
        a.op24(enc::rrr(0, 0, 0, 6, 0, A3)); a.rsync();
    };
    // A rejected table at the top can extend past physical SRAM. Seed and
    // compare the entire addressable prefix, including both partial word edges.
    for (pointer, saved, corrupt_at) in [
        (LAST, true, false), (0x3fce_effc, false, false), (LAST + 4, false, false),
        (LAST + 2, false, false), (LAST - 2, false, false),
        (RadioSram::PHY_ROM_RAM, true, false), (LAST, true, true),
    ] {
        let base = pointer & !3;
        let count = if base < RadioSram::ROM_DATA_START { 0 } else {
            ((RadioSram::ROM_DATA_END - base) / 4).min(WORDS + u32::from(pointer != base))
        };
        let pattern: Vec<_> = (0..count).map(|i| FIRST_VALUE + i).collect();
        let snapshot = if saved { pattern.clone() } else { vec![0; WORDS as usize] };
        let mut calibrated = 0; let mut requested = 0; let mut recalibrated = 0;
        let worker = assemble(&mut Asm::new(3, A8), |a| {
            const FILL: usize = 0; const SECOND: usize = 1; const PARK: usize = 2;
            a.literal_table(); a.entry(A1, 64);
            a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
            a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0);
            a.bcci_reg_label(cc::NE, A4, 1, SECOND);
            if count != 0 {
                a.mov_reg_i32(A3, base); a.mov_reg_i32(A4, FIRST_VALUE); a.movi(A5, count as i32);
                a.base.label_assign(FILL); a.s32i(A4, A3, 0); a.addi(A3, A3, 4);
                a.addi(A4, A4, 1); a.addi(A5, A5, -1); a.bccz_reg_label(ccz::NE, A5, FILL);
            }
            write(a, object, pointer);
            write(a, r.boot_tasks[0] + radio::TCB_PRIORITY, 1);
            dispatch(a); // First sleep sees the pointer before calibration completes.
            a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
            write(a, s.phy_calibrated, 1);
            calibrated = entry + a.base.offset();
            dispatch(a); // Dispatcher samples the table and blocks in sleep before we resume.
            a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
            if saved {
                write(a, pointer, 0xfeed_0001); write(a, pointer + 4 * (WORDS - 1), 0xfeed_0002);
            }
            if corrupt_at { write(a, r.phy_table_at, LAST + 4); }
            write(a, r.request, 1);
            requested = entry + a.base.offset(); a.j_label(PARK);
            a.base.label_assign(SECOND);
            if saved { write(a, pointer + 4, 0xfeed_0003); }
            write(a, object, LAST); write(a, s.phy_calibrated, 1);
            recalibrated = entry + a.base.offset();
            a.base.label_assign(PARK);
            write(a, r.boot_tasks[0] + radio::TCB_STATE, radio::BLOCKED);
            dispatch(a); a.j(-4);
        });
        assert!(image.slot.layout.regions[0].used <= u64::from(data)
            && data + worker.len() as u32 <= STACK_TOP - image.slot.layout.stack_bytes);
        let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(67));
        assert!(m.seed_data(data, &worker));
        let deadline = m.tick_hz();
        reach(&mut m, calibrated, deadline);
        assert_eq!(ram_word(&m, r.phy_table_state), 0, "wait for the first completed calibration");
        assert_eq!(words(&m, r.phy_table, WORDS), vec![0; WORDS as usize]);
        if pointer == LAST && !corrupt_at {
            reach(&mut m, save_pc, deadline);
            let (ret, begin) = (m.cpu().ar[0], m.cpu().instret);
            reach(&mut m, ret, deadline);
            let retired = m.cpu().instret - begin;
            assert!(retired < runtime::RUNTIME_GAP_INSNS);
            eprintln!("PHY table save retired {retired} instructions from entry through return");
        }
        reach(&mut m, s.restart, deadline);
        assert_eq!(ram_word(&m, r.phy_table_state), if saved { 1 } else { 2 }, "{pointer:#010x}");
        assert_eq!(ram_word(&m, r.phy_table_at), if saved { pointer } else { 0 });
        assert_eq!(words(&m, r.phy_table, WORDS), snapshot);
        let head = ram_word(&m, r.reports);
        assert_eq!(ram_word(&m, r.reports + 4) - head, u32::from(!saved));
        if !saved {
            let at = r.reports + 12 + 12 * (head % radio::REPORT_RING_ENTRIES);
            assert_eq!(words(&m, at, 3), [u32::from(watch::FAULT_RADIO_PHY_TABLE_REFUSED),
                u32::from(watch::SITE_RADIO_SCHEDULER), pointer]);
        }
        reach(&mut m, requested, deadline);
        reach(&mut m, s.restart, deadline);
        assert_eq!(ram_word(&m, r.request), 1);
        let before = words(&m, base, count);
        if saved {
            assert_eq!((before[0], before[WORDS as usize - 1]), (0xfeed_0001, 0xfeed_0002));
        } else {
            assert_eq!(before, pattern, "refused save left ROM untouched");
        }
        let (ret, begin, tick) = (m.cpu().ar[0], m.cpu().instret, m.now());
        if corrupt_at || (!saved && base >= RadioSram::ROM_DATA_START) {
            // The unaddressable suffix cannot be read back. Watch the complete
            // destination, including a store that would leave a byte unchanged.
            let bad = if corrupt_at { LAST + 4 } else { pointer };
            assert_eq!(restart_stores.advance(&mut m, deadline, |m, store| {
                if let Some((at, width)) = store {
                    assert!(u64::from(at) + u64::from(width) <= u64::from(bad) || at >= bad + WORDS * 4,
                        "restart wrote a refused table at {at:#010x}");
                }
                m.pc() == s.started
            }), Ran::Pc);
        } else {
            reach(&mut m, s.started, deadline);
        }
        assert_eq!((ram_word(&m, r.island_status), ram_word(&m, r.generation)), (radio::ISLAND_READY, 2));
        let restored = saved && !corrupt_at;
        assert_eq!(ram_word(&m, r.phy_table_state), if restored { 1 } else { 2 });
        let expected_rom = if restored { &pattern } else { &before };
        assert_eq!(&words(&m, base, count), expected_rom, "{pointer:#010x}");
        reach(&mut m, ret, deadline);
        assert!(m.cpu().instret - begin <= s.restart_max_insns && m.now() - tick <= m.tick_hz() / 20);
        reach(&mut m, recalibrated, deadline);
        reach(&mut m, s.restart, deadline);
        assert_eq!(words(&m, r.phy_table, WORDS), snapshot, "later calibration cannot replace the snapshot");
        assert_eq!(ram_word(&m, r.phy_table_state), if restored { 1 } else { 2 });
        let until = m.now() + m.tick_hz() / 100;
        assert_eq!(m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }), Ran::Tick);
        let mut out = Vec::new(); m.take_out(&mut out); m.mark();
        let mut reader = watch::Reader::new(); let mut reports = Vec::new();
        for byte in out {
            if let Some(frame) = reader.push(byte.byte) {
                if let watch::Event::Fault { code, site, arg } = frame.unwrap().event {
                    assert_eq!(site, watch::SITE_RADIO_SCHEDULER); reports.push((code, arg as u32));
                }
            }
        }
        let mut expected = vec![(watch::FAULT_RADIO_STARTED, 1)];
        if !restored { expected.push((watch::FAULT_RADIO_PHY_TABLE_REFUSED, if corrupt_at { LAST + 4 } else { pointer })); }
        expected.push((watch::FAULT_RADIO_STARTED, 0x0101_0002));
        assert_eq!(reports, expected);
    }
}

#[test]
fn planned_station_restart_bounds_include_the_optional_start_task() {
    // The planned slot uses the small archive. Substitute the measured station
    // pack's IRAM, DATA, BSS and gate counts; both windows end before a 16-aligned t0.
    let (profile, slot, _) = planned_slot();
    let segment_words = |kind| u64::from(profile.island.segments.iter().find(|s| s.kind == kind).unwrap().bytes / 4);
    let fixture_copy = segment_words(SegmentKind::Iram) + segment_words(SegmentKind::Data) + u64::from(profile.island.header.gate_count);
    // BSS grows from 9,714 to 9,948 bytes: 2 bytes of word alignment + the 232-byte stand-in record.
    let production = slot.symbols.radio.as_ref().unwrap().restart_max_insns
        + 10 * (42_680 / 4 + 4_464 / 4 + 13 - fixture_copy) + 7 * (9_948 / 4 - segment_words(SegmentKind::Bss));
    eprintln!("production restart bound {production} retired instructions");
    // 418,614 + 7 * (2,487 - 2,428) BSS words at 160 MHz; 240 MHz's two power-on
    // stretches add 800 cycles and one stretch's 15 instructions.
    assert_eq!(production, 419_842, "planned station clear, copy, BSS and gate install");
    assert!(production < u64::from(fab_elec_mcu_emit_s3::CPU_HZ) / 20);

    // Same 16-slot plan, now opting in with a 4 KiB stack. The 56-byte TCB,
    // 4 bytes before the config/table and 160 config bytes consume 208 more
    // bytes after reusing 12 bytes of padding before the idle stack.
    let source = app("fn setup() {}");
    let module = module::load(&source.module, &chip::profile()).unwrap();
    let start = Radio { start: Some(fab_elec_mcu_emit_s3::image::StartTask { priority: 3, stack_bytes: 4096 }),
        ..profile.clone() };
    let started = build_slot(&module, &SlotOptions { radio: Some(start), ..SlotOptions::default() }).unwrap();
    let clear_words = |s: &Slot| s.layout.radio.as_ref().unwrap().clear_spans().iter()
        .map(|span| (span.end - span.start) / 4).sum::<u32>();
    assert_eq!(clear_words(&started) - clear_words(slot), (4096 + 208) / 4);
    let extra = started.symbols.radio.as_ref().unwrap().restart_max_insns - slot.symbols.radio.as_ref().unwrap().restart_max_insns;
    assert_eq!(extra, 8 * 1076 + 10 * 40 + 54); // one boot frame, zero additional install feeds
    assert_eq!(production + extra, 428_904); // 427,676 + the same 59 * 7 BSS instructions + 815
}

/// phy_init.c's 10 us power-up wait in CPU cycles, fed at least every 1,600 cycles: one
/// stretch at 160 MHz, two at 240 MHz.
const POWER_ON_STRETCHES: usize = (fab_elec_mcu_emit_s3::CPU_HZ / 100_000).div_ceil(1600) as usize;

const REQUEST_WORKER_DATA: u32 = STACK_TOP - 0x4000;
const REQUEST_WORKER_ENTRY: u32 = IRAM_START + (REQUEST_WORKER_DATA - SRAM_START);

mod start_task {
    use super::*;
    use fab_elec_mcu_emit_s3::image::StartTask;
    use fab_elec_mcu_s3::{Ctx, ForeignBus, Next, VirtualFn, VirtualFns};
    use std::sync::{Arc, OnceLock};

    const ROOTS: [&str; 3] = ["esp_wifi_init_internal", "esp_wifi_set_mode", "esp_wifi_start"];
    const BACKOFF_MS: u32 = 2;

    fn fixture() -> &'static (Image, Island) {
        static FIXTURE: OnceLock<(Image, Island)> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let (_, archive) = island_fixture::station_archive_with_virtual_roots(vec![0; 4], 4, &ROOTS);
            let island = radio::link_pack(&[Archive { name: "station.a", data: &archive }]).unwrap();
            let radio = Radio { start: Some(StartTask { priority: 3, stack_bytes: 4096 }),
                restart: RestartPolicy { base_ms: BACKOFF_MS, ..RestartPolicy::PROVISIONAL },
                task_capacity: 3, irq_stack_bytes: 2048, heap_bytes: 16 * 1024, ..profile(island.binding()) };
            let source = app("fn setup() {}");
            let module = module::load(&source.module, &chip::profile()).unwrap();
            (build_image(&module, &board(radio), &SlotOptions::default()).unwrap(), island)
        })
    }

    fn init(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
        let slot = &fixture().0.slot;
        let r = slot.layout.radio.as_ref().unwrap();
        let config = r.wifi_config.unwrap();
        assert_eq!(ctx.args[0], config);
        assert_eq!((config, config % 8), (r.osi_table + 120 * 4, 0));
        for (offset, value) in [(0, r.osi_table), (4, 52), (8, 1), (56, 10), (124, 32),
            (152, 3584), (156, 0x1f2f_3f4f)] {
            assert_eq!(bus.sram_word(config + offset), Some(value), "config byte {offset}");
        }
        let symbols = &slot.symbols.radio.as_ref().unwrap().adapter_symbols;
        for (i, &(field, _)) in radio::adapter::CRYPTO_FIELDS.iter().enumerate() {
            let address = symbols.iter().find(|s| s.0 == format!("wifi_crypto_fault_{field}")).unwrap().1;
            assert_eq!(bus.sram_word(config + 12 + i as u32 * 4), Some(address), "{field}");
        }
        Next::Return(0)
    }

    fn set_mode<const FAILURES: u32>(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
        assert_eq!(ctx.args[0], 1);
        let r = fixture().0.slot.layout.radio.as_ref().unwrap();
        if bus.sram_word(r.generation).unwrap() <= FAILURES {
            if ctx.step == 0 {
                // A subsequent init can see the magic again only after a fresh config copy.
                return Next::Effect(|_, bus| {
                    let config = fixture().0.slot.layout.radio.as_ref().unwrap().wifi_config.unwrap();
                    bus.write(config + 156, Width::W, 0, 0).unwrap();
                });
            }
            return Next::Return(0x3002);
        }
        Next::Return(0)
    }

    fn start(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
        let (image, island) = fixture();
        let r = image.slot.layout.radio.as_ref().unwrap();
        let row = radio::adapter::OSI_FIELDS.iter().position(|&(field, _)| field == "_event_post").unwrap();
        match ctx.step {
            0 => Next::Call { target_at: r.osi_table + row as u32 * 4,
                args: [island.glue["wifi_event_base_name"], 2, 0, 0, u32::MAX, 0] },
            1 if ctx.result == Some(0) => Next::Return(0),
            _ => Next::Stop("station start's event post failed"),
        }
    }

    fn run<const FAILURES: u32>() {
        let (image, island) = fixture();
        let r = image.slot.layout.radio.as_ref().unwrap();
        let symbols = image.slot.symbols.radio.as_ref().unwrap();
        let entries = ROOTS.map(|name| island.entries[island.root_names.iter().position(|root| root == name).unwrap()]);
        let bodies: [fn(&Ctx, &dyn ForeignBus) -> Next; 3] = [init, set_mode::<FAILURES>, start];
        let funcs = ROOTS.into_iter().zip(entries).zip(bodies).map(|((name, address), body)| VirtualFn {
            name: name.into(), address, bytes: 16, frame: 144, body,
        }).collect();
        let v = VirtualFns::new(entries.map(|at| at..at + 16).into(), funcs).unwrap();
        let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(53))
            .with_foreign(Arc::new(v));
        let deadline = m.tick_hz();
        reach(&mut m, entries[0], deadline);
        reach(&mut m, entries[1], deadline);
        assert_eq!(ram_word(&m, r.generation), 1);
        let mut expected = vec![(watch::FAULT_RADIO_STARTED, 1)];
        for generation in 1..=FAILURES {
            reach(&mut m, symbols.restart, deadline);
            assert_eq!((ram_word(&m, r.radio_state), ram_word(&m, r.stop_cause)),
                (radio::RADIO_STOPPED, u32::from(watch::FAULT_RADIO_START_FAILED)));
            let stopped_at = m.now();
            if generation == 2 {
                // The first retry is immediate. On the second stop policy must return with
                // a real deadline and leave the same generation stopped until it expires.
                let ret = m.cpu().ar[0];
                reach(&mut m, ret, deadline);
                assert_eq!(ram_word(&m, r.radio_state), radio::RADIO_STOPPED);
                let due = u64::from(ram_word(&m, r.next_start)) | u64::from(ram_word(&m, r.next_start + 4)) << 32;
                assert!(due != 0 && due != u64::MAX);
            }
            reach(&mut m, entries[0], deadline);
            if generation == 2 { assert!(m.now() - stopped_at >= m.tick_hz() * u64::from(BACKOFF_MS) / 1000); }
            assert_eq!(ram_word(&m, r.generation), generation + 1);
            reach(&mut m, entries[1], deadline);
            let marker = generation << 24 | u32::from(watch::FAULT_RADIO_START_FAILED) << 16 | (generation + 1);
            expected.extend([(watch::FAULT_RADIO_START_FAILED, (2 << 24) | 0x3002),
                (watch::FAULT_RADIO_STARTED, marker as i32)]);
        }
        reach(&mut m, entries[2], deadline);
        let drain = image.slot.map.runtime.iter().find(|r| r.name == "radio_events").unwrap();
        reach(&mut m, drain.start, deadline);
        assert_eq!(ram_word(&m, r.boot_tasks[0] + radio::TCB_STATE), radio::BLOCKED);
        let until = m.now() + m.tick_hz() / 100;
        let reports: Vec<_> = watch_events_until(&mut m, until).into_iter().filter_map(|event| match event {
            watch::Event::Fault { code, arg, .. } => Some((code, arg)),
            watch::Event::Lost { .. } => panic!("lost start task report"),
            _ => None,
        }).collect();
        expected.push((watch::FAULT_RADIO_EVENT, 2));
        assert_eq!(reports, expected);
    }

    #[test]
    fn station_starts_and_parks() { run::<0>(); }

    #[test]
    fn failed_set_mode_restarts_at_init() { run::<1>(); }

    #[test]
    fn repeated_start_failure_waits_for_the_backoff_deadline() { run::<2>(); }
}

#[test]
fn virtual_station_posts_through_osi_and_dispatcher_drains_to_watch() {
    use fab_elec_mcu_s3::{Ctx, ForeignBus, Next, VirtualFn, VirtualFns};
    use std::sync::{Arc, OnceLock};

    const SIZE: usize = 2048;
    fn payload() -> Vec<u8> { (0..SIZE).map(|i| (i ^ (i >> 8)) as u8).collect() }

    fn fixture() -> &'static (Image, Island) {
        static FIXTURE: OnceLock<(Image, Island)> = OnceLock::new();
        FIXTURE.get_or_init(|| {
            let mut data = vec![0];
            data.extend(payload());
            let (_, archive) = island_fixture::station_archive(data, 4);
            let island = radio::link_pack(&[Archive { name: "station.a", data: &archive }]).unwrap();
            let mut board = request_board(&island);
            let radio = board.radio.as_mut().unwrap();
            radio.heap_bytes = 16 * 1024;
            radio.boot_tasks[0].priority = radio.dispatcher_priority + 1;
            radio.boot_tasks[0].stack_bytes = 4096;
            radio.boot_tasks[0].args[0] = island.glue["wifi_event_base_name"];
            radio.boot_tasks[0].args[1] = island.segments.iter().find(|s| s.kind == SegmentKind::Data).unwrap().address + 1;
            let source = app("fn setup() {}");
            let module = module::load(&source.module, &chip::profile()).unwrap();
            (build_image(&module, &board, &SlotOptions::default()).unwrap(), island)
        })
    }

    fn post(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
        let osi = |name| {
            let index = radio::adapter::OSI_FIELDS.iter().position(|&(field, _)| field == name).unwrap();
            fixture().0.slot.layout.radio.as_ref().unwrap().osi_table + index as u32 * 4
        };
        match ctx.step {
            0 => Next::Call { target_at: osi("_event_post"), args: [ctx.args[0], 2, ctx.args[1], SIZE as u32, u32::MAX, 0] },
            1 if ctx.result == Some(0) => Next::Call { target_at: osi("_event_post"), args: [0, 2, 0, 0, 0, 0] },
            2 if ctx.result == Some(0) => Next::Call { target_at: osi("_task_delay"), args: [u32::MAX, 0, 0, 0, 0, 0] },
            _ => Next::Stop("post failed or the parked station resumed"),
        }
    }

    let (image, island) = fixture();
    let entry = REQUEST_WORKER_ENTRY;
    let functions = VirtualFns::new(vec![entry..entry + 32], vec![VirtualFn {
        name: "station".into(), address: entry, bytes: 32, frame: 144, body: post,
    }]).unwrap();
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(53))
        .with_foreign(Arc::new(functions));
    let limit = m.tick_hz();
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let take = symbols.gate_reports.iter().find(|g| g.name == "event_take").unwrap();
    let free = symbols.gate_reports.iter().find(|g| g.name == "free").unwrap();
    assert!(take.max_insns + 12 <= symbols.masked_gate_insns && free.max_insns + 14 <= symbols.masked_gate_insns);
    let routine = |name| slot.map.runtime.iter().find(|r| r.name == name).unwrap().start;
    reach(&mut m, entry, limit);
    reach(&mut m, routine("radio_events"), limit);
    assert_eq!(ram_word(&m, r.boot_tasks[0] + radio::TCB_STATE), radio::BLOCKED);
    let source = island.segments.iter().find(|s| s.kind == SegmentKind::Data).unwrap().address + 1;
    let (ret, start) = (m.cpu().ar[0], m.cpu().instret);
    let mut nested = 0;
    for expected in [Some(2), Some(u32::MAX), None] {
        reach(&mut m, take.address, limit);
        assert_eq!(m.cpu().ps & 15, 15);
        let (back, begin) = (m.cpu().ar[0], m.cpu().instret);
        reach(&mut m, back, limit);
        let node = m.cpu().ar[2];
        assert_eq!(node != 0, expected.is_some());
        let cost = m.cpu().instret - begin;
        assert!(cost <= take.max_insns);
        nested += cost;
        if let Some(id) = expected {
            assert_eq!(ram_word(&m, node + 8), 2);
            if id == 2 {
                assert_ne!(node + 16, source);
                assert_eq!(ram_word(&m, node + 12), SIZE as u32);
                assert_eq!(ram_bytes(&m, &(node + 16..node + 16 + SIZE as u32)), payload());
            } else { assert_eq!(ram_word(&m, node + 12), 0); }
            reach(&mut m, routine("watch_event"), limit);
            assert_eq!(m.cpu().ps & 15, 15);
            assert_eq!(m.cpu().ar[5], id);
            let (back, begin) = (m.cpu().ar[0], m.cpu().instret);
            reach(&mut m, back, limit);
            nested += m.cpu().instret - begin;
            reach(&mut m, free.address, limit);
            assert_eq!((m.cpu().ps & 15, m.cpu().ar[2], m.cpu().ar[1]), (15, node, r.irq_stack.top));
            let (back, begin) = (m.cpu().ar[0], m.cpu().instret);
            reach(&mut m, back, limit);
            let cost = m.cpu().instret - begin;
            assert!(cost <= free.max_insns);
            nested += cost;
        }
    }
    reach(&mut m, ret, limit);
    assert_eq!(m.cpu().instret - start - nested, 13 + 2 * 50 + 24, "entry/return, two reports and the empty take");
    reach(&mut m, routine("radio_events"), limit);
    let (ret, start) = (m.cpu().ar[0], m.cpu().instret);
    reach(&mut m, ret, limit);
    assert_eq!(m.cpu().instret - start, 5, "empty head check after the last take, excluding caller");
    let until = m.now() + m.tick_hz() / 100;
    let reports: Vec<_> = watch_events_until(&mut m, until).into_iter().filter_map(|event| match event {
        watch::Event::Fault { code, site: watch::SITE_RADIO_SCHEDULER, arg } => Some((code, arg)),
        watch::Event::Lost { .. } => panic!("lost station report"),
        _ => None,
    }).collect();
    assert_eq!(reports, [(watch::FAULT_RADIO_STARTED, 1), (watch::FAULT_RADIO_EVENT, 2), (watch::FAULT_RADIO_EVENT, -1)]);
}

#[test]
fn radio_start_feeds_during_a_large_boot_install() {
    use fab_elec_mcu_emit32::xtensa::{assemble, enc, reg::*, Asm};
    const TASKS: u32 = 38;
    let island = island_fixture::small_island();
    let task = BootTask { entry: REQUEST_WORKER_ENTRY, abi: radio::TaskAbi::Call0, priority: 1,
        stack_bytes: radio::MIN_STACK_BYTES, args: [1, 2, 3, 4, 5, 6] };
    let radio = Radio { dispatcher_priority: 2, task_capacity: TASKS + 2,
        boot_tasks: vec![task; TASKS as usize], ..profile(island.binding()) };
    let app = app("var booted: i32 = 0\nfn setup() { booted = 1 }");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let image = build_image(&module, &board(radio), &SlotOptions::default()).unwrap();
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let boot = s.blobs.iter().find(|b| b.name == "radio_boot").unwrap();
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(53));
    // Each task parks its own TCB through the scheduler, so a healthy boot reaches idle.
    let worker = assemble(&mut Asm::new(0, A3), |a| {
        a.literal_table(); a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        a.mov_reg_i32(A2, r.current); a.l32i(A2, A2, 0);
        a.mov_reg_i32(A3, s.block); a.jx(A3);
    });
    assert!(slot.layout.regions[0].used <= u64::from(REQUEST_WORKER_DATA)
        && REQUEST_WORKER_DATA + worker.len() as u32 <= STACK_TOP - slot.layout.stack_bytes);
    assert!(m.seed_data(REQUEST_WORKER_DATA, &worker));

    let deadline = m.tick_hz();
    reach(&mut m, slot.symbols.setup_call, deadline);
    let stores = StoreWatch::new(&image, &[], vec![]);
    let mut install_words = 0u64;
    let mut feeds = Vec::new();
    // Include the preceding setup feed and the first feed after the installer returns.
    assert_eq!(stores.advance(&mut m, deadline, |m, store| {
        let Some((address, 4)) = store else { return false };
        let installing = (s.start..boot.end).contains(&m.pc());
        if installing && (SRAM_START..STACK_TOP).contains(&address) {
            install_words += 1;
        }
        if address != 0x6000_80ac { return false; }
        feeds.push((m.cpu().instret, install_words));
        !installing && install_words != 0
    }), Ran::Pc);
    assert!(3 * install_words > runtime::RUNTIME_GAP_INSNS, "the stores alone would exceed the unfed budget");
    assert_eq!(feeds.first().map(|f| f.1), Some(0), "observe the feed before the first store");
    assert!(feeds.windows(2).all(|f| f[1].1 - f[0].1 <= 128), "feed within 128 stores across both install lists");
    let longest = feeds.windows(2).map(|f| f[1].0 - f[0].0).max().unwrap();
    assert!(longest < runtime::RUNTIME_GAP_INSNS, "largest feed gap across RADIO_START was {longest} instructions");
    eprintln!("RADIO_START installed {install_words} words, largest feed gap {longest} retired instructions");
    reach(&mut m, s.idle, deadline);
    assert_eq!(ram_word(&m, r.task_count), TASKS + 2);
    assert_eq!(ram_word(&m, r.radio_state), radio::RADIO_RUNNING);
}

fn request_board(island: &Island) -> Board {
    board(Radio { dispatcher_priority: 2, task_capacity: 3, irq_stack_bytes: 2048,
        boot_tasks: vec![BootTask { entry: REQUEST_WORKER_ENTRY, abi: radio::TaskAbi::Windowed,
            priority: 3, stack_bytes: 2048, args: [0; 6] }], ..profile(island.binding()) })
}

// Run the workload in generation one, request once, and park the worker in both generations.
fn seed_request_worker(m: &mut S3Machine, slot: &Slot, labels: usize,
    mut workload: impl FnMut(&mut fab_elec_mcu_emit32::xtensa::Asm)) -> u32
{
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, enc, reg::*, Asm};
    let r = slot.layout.radio.as_ref().unwrap();
    let park = labels;
    let mut requesting = 0;
    let worker = assemble(&mut Asm::new(labels + 1, A8), |a| {
        a.literal_table(); a.entry(A1, 64);
        a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0);
        a.bcci_reg_label(cc::NE, A4, 1, park);
        workload(a);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        a.mov_reg_i32(A3, r.request); a.movi(A4, 1);
        requesting = REQUEST_WORKER_ENTRY + a.base.offset();
        a.s32i(A4, A3, 0);
        a.base.label_assign(park);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        for (tcb, state) in [(r.boot_tasks[0], radio::BLOCKED), (r.dispatcher, radio::READY)] {
            a.mov_reg_i32(A3, tcb); a.movi(A4, state as i32); a.s32i(A4, A3, radio::TCB_STATE);
        }
        a.movi(A4, 1 << radio::YIELD_LINE); a.wsr(A4, 226);
        a.op24(enc::rrr(0, 0, 0, 6, 0, A3)); a.rsync(); a.j(-4);
    });
    assert!(slot.layout.regions[0].used <= u64::from(REQUEST_WORKER_DATA)
        && REQUEST_WORKER_DATA + worker.len() as u32 <= STACK_TOP - slot.layout.stack_bytes);
    assert!(m.seed_data(REQUEST_WORKER_DATA, &worker));
    requesting
}

fn watch_events_until(m: &mut S3Machine, until: u64) -> Vec<watch::Event> {
    assert_eq!(m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0,
        on_drive: false }), Ran::Tick, "watch observation stopped at {:?}", m.status());
    watch_events(m, "restart")
}

fn watch_events(m: &mut S3Machine, at: &str) -> Vec<watch::Event> {
    let mut out = Vec::new(); m.take_out(&mut out);
    let mut reader = watch::Reader::new();
    let mut events = Vec::new();
    for byte in out {
        assert_eq!(byte.link, 0, "{at}: watch output reaches USB");
        if let Some(frame) = reader.push(byte.byte) {
            let frame = frame.unwrap_or_else(|err| panic!("{at}: invalid USB watch frame: {err:?}"));
            assert_eq!(frame.seq, events.len() as u8, "{at}: continuous watch sequence through restart");
            events.push(frame.event);
        }
    }
    events
}

struct RadioLinkFrames {
    /// (epoch tick, radio generation) from each Hello, in wire order.
    hellos: Vec<(u32, u32)>,
    starts: Vec<(u16, u8)>,
    credits: Vec<u16>,
}

fn observe_radio_link(m: &mut S3Machine, reader: &mut watch::Reader, slot: &Slot, ms: u64) -> RadioLinkFrames {
    let until = m.now() + m.tick_hz() * ms / 1000;
    loop {
        match m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }) {
            Ran::Tick => break,
            Ran::Barrier => m.mark(),
            ran => panic!("watch link stopped at {ran:?}: {:?}", m.status()),
        }
    }
    let mut hellos = Vec::new();
    let mut starts = Vec::new();
    let mut credits = Vec::new();
    let mut out = Vec::new(); m.take_out(&mut out);
    for byte in out {
        assert_eq!(byte.link, 0);
        if let Some(frame) = reader.push(byte.byte) {
            let frame = frame.expect("valid watch frame");
            match frame.event {
                watch::Event::Hello { image, generation, .. } => {
                    assert_eq!(image, slot.image_id);
                    hellos.push((frame.tick, generation));
                }
                watch::Event::Fault { code: watch::FAULT_RADIO_STARTED, arg, .. } => {
                    let start = watch::radio_start(arg);
                    starts.push((start.generation, start.cause));
                }
                watch::Event::EthCredit { count } => credits.push(count),
                _ => {}
            }
        }
    }
    RadioLinkFrames { hellos, starts, credits }
}

fn restart_wire(seq: u8, epoch: u32, extra_payload: bool) -> Vec<u8> {
    let mut wire = [0; watch::MAX_WIRE];
    let mut n = watch::encode_radio_restart(seq, epoch, &mut wire);
    if extra_payload {
        let mut body = [0; 8];
        assert_eq!(watch::cobs_decode(&wire[..n - 1], &mut body), Some(7));
        body[6] = 0x5a;
        body[7] = watch::crc8(&body[..7]);
        n = watch::cobs_encode(&body, &mut wire).unwrap() + 1;
        wire[n - 1] = 0;
    }
    wire[..n].to_vec()
}

fn send_host_bytes(m: &mut S3Machine, wire: &[u8]) {
    for &byte in wire { m.link_in(0, byte); }
}

#[test]
fn a_radio_image_takes_a_restart_command_from_the_host_link() {
    let island = island_fixture::small_island();
    // Writes give the host-open detector pending TX while the host port is closed.
    let app = pin_app("let out = output(D33)\nlet tick = every(ms(20), fn() { write(out, 1) })");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let image = build_image(&module, &board(profile(island.binding())), &SlotOptions::default()).unwrap();
    let slot = &image.slot;
    assert!(slot.layout.eth.is_none());
    let link = slot.layout.link.as_ref().unwrap();
    let radio = slot.layout.radio.as_ref().unwrap();
    let rx_return = slot.map.runtime.iter().find(|r| r.name == "eth_rx").unwrap().end - 2;
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(53));
    let deadline = m.tick_hz();
    reach(&mut m, slot.symbols.t0_ready, deadline);

    let mut reader = watch::Reader::new();
    let mut observe = |m: &mut S3Machine, ms| {
        let frames = observe_radio_link(m, &mut reader, slot, ms);
        (frames.hellos, frames.starts)
    };
    let send = |m: &mut S3Machine, seq, epoch, extra_payload| {
        send_host_bytes(m, &restart_wire(seq, epoch, extra_payload));
    };
    let (hellos, starts) = observe(&mut m, 3);
    let (hellos, generations): (Vec<_>, Vec<_>) = hellos.into_iter().unzip();
    let boot = ram_word(&m, link.epoch);
    assert_eq!(hellos, [boot], "boot Hello carries the stored epoch");
    assert_eq!(generations, [0], "the radio-only boot Hello precedes the first radio start");
    assert_eq!(starts, [(1, 0)], "the island started at boot");

    send(&mut m, 0, boot, false);
    // Before the first 20 ms timer, USB alone must wake the dispatcher.
    let deadline = m.now() + m.tick_hz() / 200;
    reach(&mut m, rx_return, deadline);
    assert_eq!((ram_word(&m, radio.request), ram_word(&m, radio.generation)), (1, 1),
        "RX publishes the request before the radio restart in this sleep");
    reach(&mut m, slot.symbols.radio.as_ref().unwrap().started, deadline);
    assert_eq!(ram_word(&m, radio.generation), 2, "the same idle turn takes the request");
    assert_eq!(observe(&mut m, 25), (vec![], vec![(2, 1)]), "current epoch requests one restart");

    let rejected = |m: &S3Machine| (ram_word(m, radio.request), ram_word(m, radio.generation),
        ram_word(m, link.eth_stale), ram_word(m, link.rx_bad));
    send(&mut m, 1, boot.wrapping_add(1), false);
    assert_eq!(observe(&mut m, 25), (vec![], vec![]));
    assert_eq!(rejected(&m), (0, 2, 1, 0), "a wrong epoch counts only as stale");
    send(&mut m, 2, boot, true);
    assert_eq!(observe(&mut m, 25), (vec![], vec![]));
    assert_eq!(rejected(&m), (0, 2, 1, 1), "a valid CRC cannot admit a payload byte");

    m.set_usb_host_listening(false);
    assert_eq!(observe(&mut m, 360), (vec![], vec![]));
    assert_eq!(ram_word(&m, link.stall_state), 2, "the stalled writer armed the host-open edge");
    m.set_usb_host_listening(true);
    let (hellos, starts) = observe(&mut m, 30);
    let (hellos, generations): (Vec<_>, Vec<_>) = hellos.into_iter().unzip();
    let reopened = ram_word(&m, link.epoch);
    assert_eq!(hellos, [reopened], "one host-open edge sends one fresh Hello");
    assert_eq!(generations, [2], "a late host learns the radio's generation from the fresh Hello");
    assert!(starts.is_empty());
    assert_ne!(reopened, boot);
    send(&mut m, 3, boot, false);
    assert_eq!(observe(&mut m, 25), (vec![], vec![]));
    assert_eq!(rejected(&m), (0, 2, 2, 1), "reopening invalidates the old epoch");
    send(&mut m, 4, reopened, false);
    assert_eq!(observe(&mut m, 25), (vec![], vec![(3, 1)]), "the fresh echo requests the next start");
    assert_eq!(rejected(&m), (0, 3, 2, 1));
    assert!(m.last_reset().is_none(), "radio restarts preserve the USB lifetime and the app");
}

#[test]
fn a_mixed_image_takes_a_restart_command_beside_its_ethernet_slots() {
    let island = island_fixture::small_island();
    let app = pin_app("
let packet = bytes(1514)
let release = input(D4, pull_down)
var drained: i32 = 0
let network = eth(1, fn() {})
let timer = every(ms(4), fn() {
    if drained == 0 {
        if read(release) != 0 { drained = eth_recv(packet, 0) }
    }
})");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let image = build_image(&module, &board(profile(island.binding())), &SlotOptions::default()).unwrap();
    let slot = &image.slot;
    let eth = slot.layout.eth.as_ref().expect("mixed image has Ethernet slots");
    let radio = slot.layout.radio.as_ref().expect("mixed image has a radio island");
    let link = slot.layout.link.as_ref().expect("Ethernet and radio share the host link");
    assert_eq!(eth.count, 1, "one unread ETH frame fills this fixture's slot ring");
    let slot_len = |m: &S3Machine| ram_word(m, eth.slots) as u16;
    let counts = |m: &S3Machine| [link.rx_refused, link.rx_bad, link.eth_stale].map(|addr|
        u64::from(ram_word(m, addr)) | (u64::from(ram_word(m, addr + 4)) << 32));
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(53));
    m.set_input(4, false);
    let deadline = m.tick_hz();
    reach(&mut m, slot.symbols.t0_ready, deadline);
    let mut reader = watch::Reader::new();
    // Two app timer periods plus USB service let both idle and its reports reach the host.
    let mut observe = |m: &mut S3Machine| observe_radio_link(m, &mut reader, slot, 10);
    let boot = observe(&mut m);
    assert_eq!(boot.hellos.len(), 1, "boot sends one Hello");
    let (epoch, generation) = boot.hellos[0];
    assert_eq!(epoch, ram_word(&m, link.epoch), "Hello's tick is the epoch checked by RX");
    assert_eq!(generation, 0, "the mixed-image boot Hello precedes the first radio start");
    assert_eq!(boot.credits, [1], "boot grants the host its one receive slot");
    assert_eq!(boot.starts, [(1, 0)], "boot emits the first start marker");
    assert_eq!(ram_word(&m, radio.generation), 1, "the island has started once");

    let before = (ram_word(&m, eth.received), ram_word(&m, eth.owed));
    let first = restart_wire(0, epoch, false);
    // Send just the COBS code before the kind, so the reservation itself is observable.
    send_host_bytes(&mut m, &first[..1]);
    let partial = observe(&mut m);
    assert!(partial.starts.is_empty(), "a partial request cannot restart the radio");
    assert!(partial.credits.is_empty(), "reserving a slot earns no Ethernet credit");
    assert_eq!(ram_word(&m, eth.rx_slot), eth.slots, "the first byte reserves the Ethernet slot");
    assert_eq!(slot_len(&m), 0, "reservation leaves the frame unpublished");
    send_host_bytes(&mut m, &first[1..]);
    let accepted = observe(&mut m);
    assert_eq!(accepted.starts, [(2, 1)], "the current epoch starts generation two by host request");
    assert!(accepted.credits.is_empty(), "a restart earns no Ethernet credit");
    assert_eq!(slot_len(&m), 0, "the complete restart publishes no Ethernet payload");
    assert_eq!((ram_word(&m, eth.received), ram_word(&m, eth.owed)), before,
        "a restart leaves the received and owed counts unchanged");
    assert_eq!(ram_word(&m, eth.rx_slot), 0, "the delimiter releases the provisional reservation");
    assert_eq!(counts(&m), [0, 0, 0], "the accepted command incurs no refusal, bad frame or stale epoch");

    send_host_bytes(&mut m, &restart_wire(1, epoch.wrapping_add(1), false));
    assert!(observe(&mut m).starts.is_empty(), "a wrong epoch emits no start marker");
    assert_eq!(counts(&m), [0, 0, 1], "a wrong epoch increments only the stale counter");
    assert_eq!((ram_word(&m, radio.request), ram_word(&m, radio.generation)), (0, 2),
        "a stale restart leaves the request clear and the generation intact");

    let payload = [0x5a; watch::ETH_MIN];
    let mut wire = [0; watch::ETH_WIRE];
    let n = watch::encode_eth(2, epoch, &payload, &mut wire).expect("minimum-size ETH frame encodes");
    send_host_bytes(&mut m, &wire[..n]);
    let occupied = observe(&mut m);
    assert!(occupied.credits.is_empty(), "the empty ETH handler leaves its frame unread");
    assert_eq!(slot_len(&m), payload.len() as u16, "the sole slot holds the published ETH frame");

    let retry = restart_wire(3, epoch, false);
    send_host_bytes(&mut m, &retry);
    assert!(observe(&mut m).starts.is_empty(), "a full ring refuses the restart before dispatch");
    assert_eq!(counts(&m), [1, 0, 1], "a full slot ring adds exactly one reservation refusal");
    assert_eq!((ram_word(&m, radio.request), ram_word(&m, radio.generation)), (0, 2),
        "the refused command neither sets the request nor advances the generation");
    assert_eq!(slot_len(&m), payload.len() as u16, "the refusal preserves the unread frame");

    m.set_input(4, true);
    let freed = observe(&mut m);
    assert_eq!(freed.credits, [1], "the app's timer receives the frame and returns one credit");
    assert_eq!(slot_len(&m), 0, "eth_recv frees the slot for the retry");
    assert!(freed.hellos.is_empty(), "freeing a slot keeps the same link lifetime");
    assert!(freed.starts.is_empty(), "a free slot alone cannot replay the refused command");
    send_host_bytes(&mut m, &retry);
    assert_eq!(observe(&mut m).starts, [(3, 1)], "the identical wire frame starts generation three by request");
    assert_eq!(counts(&m), [1, 0, 1], "retry succeeds without another receive error");
    assert!(m.last_reset().is_none(), "the device stays up through both radio restarts");
}

#[test]
fn restart_keeps_the_app_event_stream_of_a_radio_off_control() {
    use fab_elec_mcu_emit32::xtensa::{cc, reg::*};
    const PERIOD_MS: u64 = 4;
    const PERIODS: u64 = 15;
    let app = pin_app(&format!("
let out = output(D33)
let aux = output(D4)
var turns: i32 = 0
let tick = every(ms({PERIOD_MS}), fn() {{
    turns += 1
    write(out, turns & 1)
    write(aux, (turns + 1) & 1)
}})"));
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let options = SlotOptions { sites: app.sites.iter().map(|s| (s.line, s.col)).collect(),
        ..SlotOptions::default() };
    let island = island_fixture::small_island();
    let mut board = request_board(island);
    let radio = build_image(&module, &board, &options).unwrap();
    board.radio = None;
    let control = build_image(&module, &board, &options).unwrap();
    let r = radio.slot.layout.radio.as_ref().unwrap();
    let s = radio.slot.symbols.radio.as_ref().unwrap();
    let turns = radio.slot.layout.state + 4 * module.vars().iter().find(|v| module.string(v.name) == "turns").unwrap().offset;
    let period_cycles = u64::from(fab_elec_mcu_emit_s3::CPU_HZ) * PERIOD_MS / 1000;
    assert!(s.restart_max_insns < period_cycles, "P3 restart bound must fit between timer firings");
    eprintln!("P3 restart bound {} instructions, timer period {period_cycles} CPU cycles", s.restart_max_insns);

    let p = s3_fixture::profile(69);
    let mut on = island_fixture::machine_with_island(&radio.flash, &island.image, &p);
    let mut off = island_fixture::machine_with_island(&control.flash, &[], &p);
    let requesting = seed_request_worker(&mut on, &radio.slot, 2, |a| {
        const WAIT: usize = 0;
        const TICKED: usize = 1;
        // Yield to the dispatcher and wake on its timer interrupts until three handlers have run.
        a.mov_reg_i32(A6, turns);
        a.mov_reg_i32(A3, r.boot_tasks[0]); a.movi(A4, 1); a.s32i(A4, A3, radio::TCB_PRIORITY);
        a.movi(A4, 1 << radio::YIELD_LINE); a.wsr(A4, 226); a.rsync();
        a.base.label_assign(WAIT); a.l32i(A4, A6, 0); a.bcci_reg_label(cc::GE, A4, 3, TICKED);
        a.waiti(0); a.j_label(WAIT);
        a.base.label_assign(TICKED);
    });
    // The slot CRC runs before t0. End both runs half a period after the last firing so USB drains.
    let epoch = |m: &mut S3Machine, slot: &Slot| {
        let deadline = m.tick_hz();
        reach(m, slot.symbols.t0_ready, deadline);
        m.now()
    };
    let period = on.tick_hz() * PERIOD_MS / 1000;
    let window = period * PERIODS + period / 2;
    let off_end = epoch(&mut off, &control.slot) + window;
    let on_epoch = epoch(&mut on, &radio.slot);
    let fourth_firing = on_epoch + 4 * period;
    reach(&mut on, requesting, fourth_firing);
    assert_eq!(ram_word(&on, turns), 3, "P3 requests after the third handler");
    reach(&mut on, s.started, fourth_firing);
    assert_eq!(ram_word(&on, turns), 3, "P3 restart finishes between the same two handlers");
    let normalize_hello = |event: &mut watch::Event, slot: &Slot| {
        // Hello identifies the CRC of this complete slot. Check it separately and retain every other field.
        if let watch::Event::Hello { image, .. } = event {
            assert_eq!(*image, slot.image_id, "P3 Hello identifies its own image");
            *image = 0;
        }
    };
    let mut expected = watch_events_until(&mut off, off_end);
    assert!(matches!(expected.first(), Some(watch::Event::Hello { .. })), "P3 control includes Hello");
    assert_eq!(expected.iter().filter(|e| matches!(e, watch::Event::Write { .. })).count() as u64,
        2 * PERIODS, "P3 control emits both writes on every firing");
    for event in &mut expected { normalize_hello(event, &control.slot); }
    let mut actual = Vec::new();
    let mut markers = Vec::new();
    let mut writes_after_restart = 0;
    for mut event in watch_events_until(&mut on, on_epoch + window) {
        match event {
            watch::Event::Fault { code, site: watch::SITE_RADIO_SCHEDULER, arg } if watch::FAULT_KERNEL_REPORT.contains(&code) => {
                if code == watch::FAULT_RADIO_STARTED { markers.push(watch::radio_start(arg)); }
            }
            watch::Event::RadioRecord { .. } => {}
            _ => {
                if markers.len() == 2 && matches!(event, watch::Event::Write { .. }) { writes_after_restart += 1; }
                normalize_hello(&mut event, &radio.slot);
                actual.push(event);
            }
        }
    }
    assert_eq!(markers, [watch::RadioStart { generation: 1, cause: 0, consecutive: 0 },
        watch::RadioStart { generation: 2, cause: 1, consecutive: 1 }], "P3 includes both starts in stream order");
    assert_eq!(actual, expected, "P3 preserves every app event, including write values, sites and order");
    assert!(writes_after_restart >= 20, "P3 observed only {writes_after_restart} writes after the restart marker");
}

const TEST_RESTART: RestartPolicy = RestartPolicy {
    base_ms: 2, ceiling_ms: 16, stable_ms: 50, hold_after_fault: false,
};

// The worker lives above allocated SRAM, so init and every cold rebuild leave its code in place.
fn policy_worker(m: &mut S3Machine, slot: &Slot, every_start: bool,
    mut body: impl FnMut(&mut fab_elec_mcu_emit32::xtensa::Asm)) {
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, enc, reg::*, Asm};
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let bytes = assemble(&mut Asm::new(3, A8), |a| {
        a.literal_table(); a.entry(A1, 64);
        if !every_start {
            a.mov_reg_i32(A3, r.generation); a.l32i(A3, A3, 0); a.bcci_reg_label(cc::NE, A3, 1, 0);
        }
        body(a);
        a.base.label_assign(0);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        a.mov_reg_i32(A2, r.boot_tasks[0]); a.mov_reg_i32(A8, s.block); a.callx0(A8); a.j(-4);
    });
    assert!(slot.layout.regions[0].used <= u64::from(REQUEST_WORKER_DATA)
        && bytes.len() <= (P2_ISR - REQUEST_WORKER_ENTRY) as usize
        && REQUEST_WORKER_DATA + bytes.len() as u32 <= STACK_TOP - slot.layout.stack_bytes
        && m.seed_data(REQUEST_WORKER_DATA, &bytes), "the policy worker fits outside all runtime allocations");
}

fn policy_word64(m: &S3Machine, lo: u32) -> u64 {
    u64::from(ram_word(m, lo)) | (u64::from(ram_word(m, lo + 4)) << 32)
}

// At "started" the marker is the newest published ring entry, before its USB drain.
fn published_start(m: &S3Machine, r: &fab_elec_mcu_emit_s3::RadioSram) -> watch::RadioStart {
    let tail = ram_word(m, r.reports + 4);
    assert_ne!(tail, ram_word(m, r.reports), "a start must publish a ring entry");
    let entry = r.reports + 12 + 12 * (tail.wrapping_sub(1) % radio::REPORT_RING_ENTRIES);
    assert_eq!((ram_word(m, entry), ram_word(m, entry + 4)),
        (u32::from(watch::FAULT_RADIO_STARTED), u32::from(watch::SITE_RADIO_SCHEDULER)),
        "the start timestamp belongs to the scheduler's marker");
    watch::radio_start(ram_word(m, entry + 8) as i32)
}

#[test]
fn failed_phy_starts_back_off_reset_after_stability_and_accept_a_request() {
    use fab_elec_mcu_emit32::xtensa::{ccz, reg::*};
    use s3_fixture::{GPIO_IN, MUX_GPIO, MUX_IE, mux_reg};
    const FAILED: u8 = watch::FAULT_RADIO_PHY_INIT_FAILED;
    let island = island_fixture::linked_island(vec![1, 3, 5], 20);
    let out = island.objects[radio::PHY_FUNS_OBJECT].start + 4;
    let app = app("let tick = every(ms(80), fn() {})");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let mut board = request_board(&island);
    for (base_ms, ceiling_ms, reason) in [(0, 16, "base_ms"), (2, 1, "ceiling_ms")] {
        board.radio.as_mut().unwrap().restart = RestartPolicy { base_ms, ceiling_ms, ..TEST_RESTART };
        let error = build_image(&module, &board, &SlotOptions::default()).err().expect("invalid restart intervals must refuse the image");
        assert!(error.message.contains(reason), "an invalid restart interval names {reason}: {}", error.message);
    }
    board.radio.as_mut().unwrap().restart = TEST_RESTART;
    let image = build_image(&module, &board, &SlotOptions::default()).unwrap();
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let profile = s3_fixture::profile(73);
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &profile);
    let deadline = m.tick_hz();
    let ticks_ms = u64::from(radio::kernel::TICKS_PER_US) * 1000;
    // The model services USB once per frame. One such sleep/wake quantum includes
    // the worker's failed init, report drain and rebuild, on top of the policy delay.
    let quantum = profile.pins.usb_service_ticks * ticks_ms * 1000 / m.tick_hz();
    let gate_word = |name: &str| {
        let idx = island.gate_names.iter().position(|g| g == name).expect("fixture has the production PHY gates");
        island.header.gate_table + 4 * idx as u32
    };
    policy_worker(&mut m, slot, true, |a| {
        // A high pin holds this generation healthy. Drop below the dispatcher
        // while waiting so its regular timer continues servicing the app.
        a.mov_reg_i32(A3, mux_reg(5)); a.mov_reg_i32(A4, MUX_GPIO | MUX_IE); a.s32i(A4, A3, 0);
        a.base.label_assign(1); a.mov_reg_i32(A3, GPIO_IN); a.l32i(A4, A3, 0); a.extui(A4, A4, 5, 1);
        a.bccz_reg_label(ccz::EQ, A4, 2);
        a.mov_reg_i32(A3, r.boot_tasks[0]); a.movi(A4, 1); a.s32i(A4, A3, radio::TCB_PRIORITY);
        a.waiti(0); a.j_label(1);
        a.base.label_assign(2);
        a.mov_reg_i32(A8, gate_word("phy_lock")); a.l32i(A8, A8, 0); a.callx8(A8);
        a.mov_reg_i32(A10, radio::kernel::PHY_OP_ENABLE); a.mov_reg_i32(A11, out);
        a.mov_reg_i32(A8, gate_word("phy_enter")); a.l32i(A8, A8, 0); a.callx8(A8);
        // Pass phy_enter's returned step directly to phy_init_done.
        a.movi(A11, -7); a.mov_reg_i32(A8, gate_word("phy_init_done")); a.l32i(A8, A8, 0); a.callx8(A8);
    });
    m.set_input(5, false);
    reach(&mut m, s.started, deadline);
    let mut previous = policy_word64(&m, r.started_at);
    let boot = watch::RadioStart { generation: 1, cause: 0, consecutive: 0 };
    assert_eq!(published_start(&m, r), boot, "P5 starts the retry history at boot");
    let mut markers = vec![boot];
    for (i, delay_ms) in [0u64, 2, 4, 8, 16, 16, 16].into_iter().enumerate() {
        if i == 4 {
            // Fork only after the dispatcher has armed the backoff alarm and blocked.
            reach(&mut m, s.report, deadline);
            assert_eq!(m.cpu().ar[2], u32::from(FAILED), "the delay fork starts at this generation's PHY failure");
            reach(&mut m, s.idle, deadline);
            let due = policy_word64(&m, r.next_start);
            assert!(due >= previous + delay_ms * ticks_ms && due != u64::MAX, "P5 forks with an actual ceiling delay pending");
            let mut manual = m.clone();
            let epoch = ram_word(&manual, slot.layout.link.as_ref().unwrap().epoch);
            send_host_bytes(&mut manual, &restart_wire(0, epoch, false));
            reach(&mut manual, s.started, deadline);
            assert_eq!(published_start(&manual, r), watch::RadioStart { generation: 6, cause: 1, consecutive: 5 },
                "a host request supersedes the pending automatic cause and counts once");
            assert!(policy_word64(&manual, r.started_at) < due, "a host request bypasses the pending backoff");
        }
        reach(&mut m, s.started, deadline);
        let now = policy_word64(&m, r.started_at);
        let expected = delay_ms * ticks_ms;
        let elapsed = now - previous;
        assert!((expected..=expected + quantum).contains(&elapsed),
            "P5 retry {} took {elapsed} ticks, expected {expected} plus at most one sleep quantum {quantum}", i + 1);
        let marker = watch::RadioStart { generation: 2 + i as u16, cause: FAILED, consecutive: 1 + i as u8 };
        assert_eq!(published_start(&m, r), marker, "P5 counts every failed PHY start including ceiling retries");
        assert_eq!(ram_word(&m, r.phy_table_state), 0, "a failed PHY init keeps the snapshot available for a successful start");
        markers.push(marker); previous = now;
    }
    m.set_input(5, true);
    let until = m.now() + (u64::from(TEST_RESTART.stable_ms) + 1) * m.tick_hz() / 1000;
    assert_eq!(m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }),
        Ran::Tick, "P5 keeps the next run healthy past stable_ms");
    assert_eq!((ram_word(&m, r.radio_state), ram_word(&m, r.generation)), (radio::RADIO_RUNNING, 8),
        "P5 measures stability in one uninterrupted RUNNING generation");
    m.set_input(5, false); reach(&mut m, s.report, deadline);
    assert_eq!(m.cpu().ar[2], u32::from(FAILED), "P5 ends the stable run with the same PHY failure");
    let failed_at = m.now();
    reach(&mut m, s.started, deadline);
    let stable_retry = watch::RadioStart { generation: 9, cause: FAILED, consecutive: 1 };
    assert_eq!(published_start(&m, r), stable_retry, "P5 stability resets the streak before counting this restart");
    assert!(m.now() - failed_at <= profile.pins.usb_service_ticks, "P5 restarts immediately after the stable run ends");
    markers.push(stable_retry); m.set_input(5, true);
    let until = m.now() + m.tick_hz() / 10;
    let mut received = Vec::new(); let mut failures = 0;
    for event in watch_events_until(&mut m, until) {
        match event {
            watch::Event::Fault { code: FAILED, arg, .. } => {
                assert_eq!((arg, received.len()), (-7, failures + 1), "the failed PHY result belongs to the preceding start");
                failures += 1;
            }
            watch::Event::Fault { code: watch::FAULT_RADIO_STARTED, site: watch::SITE_RADIO_SCHEDULER, arg } => {
                assert_eq!(failures, received.len(), "every old PHY failure precedes its recovery marker");
                received.push(watch::radio_start(arg));
            }
            watch::Event::Fault { .. } | watch::Event::Lost { .. } => panic!("unexpected PHY failure stream event {event:?}"),
            _ => {}
        }
    }
    assert_eq!(received, markers, "P5 delivers every retry and the stable reset on the host stream");
}

#[test]
fn radio_fault_restarts_after_its_record_or_waits_for_a_request_when_held() {
    let island = island_fixture::small_island();
    let app = app("let tick = every(ms(2), fn() {})");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    for hold in [false, true] {
        let mut board = request_board(island);
        board.radio.as_mut().unwrap().restart = RestartPolicy { hold_after_fault: hold, ..TEST_RESTART };
        let image = build_image(&module, &board, &SlotOptions::default()).unwrap();
        let slot = &image.slot;
        let r = slot.layout.radio.as_ref().unwrap(); let s = slot.symbols.radio.as_ref().unwrap();
        let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(75));
        let mut illegal = 0;
        policy_worker(&mut m, slot, false, |a| { illegal = REQUEST_WORKER_ENTRY + a.base.offset(); a.op24(0); });
        let deadline = m.tick_hz();
        reach(&mut m, illegal, deadline); reach(&mut m, s.restore, deadline);
        assert_eq!((ram_word(&m, r.radio_state), ram_word(&m, r.fault)), (radio::RADIO_DEAD, radio::FAULT_WORDS),
            "the fault captures its complete record before the dispatcher decides recovery");
        if hold {
            let until = m.now() + m.tick_hz() / 10;
            assert_eq!(m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }),
                Ran::Tick, "the app and USB run while the fault is held");
            assert_eq!((ram_word(&m, r.radio_state), ram_word(&m, r.generation), ram_word(&m, r.report_next)),
                (radio::RADIO_DEAD, 1, radio::FAULT_FRAMES), "hold preserves DEAD even after the final record frame");
            let epoch = ram_word(&m, slot.layout.link.as_ref().unwrap().epoch);
            send_host_bytes(&mut m, &restart_wire(0, epoch, false));
        }
        reach(&mut m, s.started, deadline);
        let recovery = watch::RadioStart { generation: 2, cause: if hold { watch::RADIO_START_CAUSE_REQUEST } else { watch::RADIO_START_CAUSE_FAULT }, consecutive: 1 };
        assert_eq!(published_start(&m, r), recovery, "recovery distinguishes an automatic fault restart from a host request");
        let until = m.now() + m.tick_hz() / 15;
        let mut collector = radio::RecordCollector::new(); let mut records = 0; let mut starts = Vec::new();
        for event in watch_events_until(&mut m, until) {
            if matches!(event, watch::Event::RadioRecord { .. }) {
                assert_eq!(starts.len(), 1, "every record frame is between the boot and recovery markers");
                if let Some(words) = collector.push(event).expect("fault frames must form one complete record") {
                    assert_eq!(radio::RadioFault::decode(&words).unwrap().pc, illegal, "the recovered record names the faulting worker PC");
                    records += 1;
                }
            } else if let watch::Event::Fault { code: watch::FAULT_RADIO_STARTED, site: watch::SITE_RADIO_SCHEDULER, arg } = event {
                starts.push(watch::radio_start(arg));
            } else if matches!(event, watch::Event::Lost { .. }) { panic!("fault recovery must preserve the host stream"); }
        }
        assert_eq!((records, starts), (1, vec![watch::RadioStart { generation: 1, cause: 0, consecutive: 0 }, recovery]),
            "one full fault record precedes exactly one recovery");
    }
}

#[test]
fn degrading_reports_precede_the_next_start_marker() {
    use fab_elec_mcu_emit32::xtensa::{assemble, reg::*, Asm};
    use s3_fixture::{pin_reg, GPIO_STATUS};
    use watch::{FAULT_KERNEL_HEAP_EXHAUSTED as HEAP, FAULT_KERNEL_TIMER_HANDLE_INVALID as INVALID,
        FAULT_KERNEL_TIMER_LOST as LOST, FAULT_KERNEL_TIMER_REFUSED as REFUSED, FAULT_KERNEL_TIMER_UNSET as UNSET};

    // Each OSI call originates in linked island DATA. The worker passes the address
    // of the installed OSI row and its arguments through this windowed entry.
    let bridge = assemble(&mut Asm::new(0, A8), |a| {
        a.entry(A1, 64); a.l32i(A8, A2, 0);
        for i in 0..4 { a.mov_n(A10 + i, A3 + i); }
        a.callx8(A8); a.mov_n(A2, A10); a.retw_n();
    });
    let island = island_fixture::linked_island(bridge, 24);
    let data = island.segments.iter().find(|s| s.kind == SegmentKind::Data).unwrap();
    let entry = IRAM_START + (data.address - SRAM_START);
    let ets = island.objects[radio::PHY_FUNS_OBJECT].start + 4;
    let app = pin_app("let out = output(D33)\nvar n: i32 = 0\nlet tick = every(ms(4), fn() { n += 1\n write(out, n & 1) })");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let mut board = request_board(&island);
    let profile = board.radio.as_mut().unwrap();
    profile.restart = RestartPolicy { hold_after_fault: true, ..TEST_RESTART };
    profile.timer_reserve = 0;
    let all_free = profile.heap_bytes - radio::kernel::heap_control_bytes(profile.heap_bytes) - 16;
    let options = SlotOptions { sites: app.sites.iter().map(|s| (s.line, s.col)).collect(), ..SlotOptions::default() };
    let image = build_image(&module, &board, &options).unwrap();
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let call = |a: &mut Asm, field: &str, args: &[u32]| {
        let row = radio::adapter::OSI_FIELDS.iter().position(|&(name, _)| name == field).unwrap();
        a.mov_reg_i32(A10, r.osi_table + row as u32 * 4);
        for (i, arg) in args.iter().enumerate() { a.mov_reg_i32(A11 + i as u8, *arg); }
        a.mov_reg_i32(A8, entry); a.callx8(A8);
    };
    let store = |a: &mut Asm, addr, value| {
        a.mov_reg_i32(A3, addr); a.mov_reg_i32(A4, value); a.s32i(A4, A3, 0);
    };
    let model = s3_fixture::profile(69);
    let root = island_fixture::machine_with_island(&image.flash, &island.image, &model);
    let deadline = root.tick_hz();
    let duration = 18 * root.tick_hz() / 1000;
    let state = r.kernel_state..r.kernel_state + 4 * r.kernel_state_words;
    board.radio = None;
    let off = build_image(&module, &board, &options).unwrap();
    let mut control = island_fixture::machine_with_island(&off.flash, &[], &model);
    reach(&mut control, off.slot.symbols.t0_ready, deadline);
    let end = control.now() + duration;
    let mut expected = watch_events_until(&mut control, end);
    for event in &mut expected { p2_check_hello(event, &off.slot); }
    assert_eq!(expected.iter().filter(|e| matches!(e, watch::Event::Write { .. })).count(), 4,
        "P4 control observes every app timer after the stop");

    let stub = s.kernel_symbols.iter().find(|sym| sym.0 == "kernel_fault").unwrap().1;
    let xip = s3_fixture::FLASH + island.header.mmu_page * 65536;
    let watch = StoreWatch::new(&image, &[s.started, s.restore, stub], vec![SRAM_START..island.window_end,
        IRAM_START..IRAM_START + (island.window_end - SRAM_START), xip..xip + island.header.image_size,
        REQUEST_WORKER_ENTRY..P2_ISR + 256]);
    for (code, isr) in [(LOST, false), (INVALID, false), (INVALID, true), (0, false)] {
        let mut m = root.clone();
        let mut armed = 0;
        policy_worker(&mut m, slot, false, |a| {
            if code != INVALID {
                call(a, "_timer_arm", &[ets, 1, 0]); // 108, unset
                call(a, "_malloc", &[u32::MAX]); // 109, refused ordinary allocation
                call(a, "_malloc", &[all_free]);
            }
            call(a, "_timer_setfn", &[ets, island.entries[0], 42]);
            if code == 0 { return; } // 96 returned with a pending timer
            call(a, "_timer_arm_us", &[ets, 1_000_000, 0]);
            if code == LOST { a.j(-4); return; } // a returning arm is a failure
            armed = REQUEST_WORKER_ENTRY + a.base.offset();
            call(a, "_wifi_int_disable", &[0]);
            a.mov_n(A7, A10); store(a, ets + 16, 1); // overwrite the armed handle
            if isr {
                call(a, "_set_isr", &[3, P2_ISR, 0]); call(a, "_ints_on", &[1 << 3]);
                store(a, GPIO_STATUS + 8, 1 << 4); store(a, pin_reg(4), (1 << 7) | (1 << 13));
                store(a, radio::kernel::INTR_MATRIX + 16 * 4, 3); store(a, GPIO_STATUS + 4, 1 << 4);
                // The ISR can enter only after this restore unmasks the GPIO line.
                let row = radio::adapter::OSI_FIELDS.iter().position(|&(n, _)| n == "_wifi_int_restore").unwrap();
                a.mov_reg_i32(A10, r.osi_table + row as u32 * 4); a.movi(A11, 0); a.mov_n(A12, A7);
                a.mov_reg_i32(A8, entry); a.callx8(A8);
            } else { call(a, "_timer_disarm", &[ets]); }
            a.j(-4);
        });
        if isr {
            let handler = assemble(&mut Asm::new(0, A8), |a| {
                a.literal_table(); a.entry(A1, 64); store(a, GPIO_STATUS + 8, 1 << 4);
                call(a, "_timer_disarm", &[ets]); a.retw_n();
            });
            let at = SRAM_START + (P2_ISR - IRAM_START);
            assert!(at + handler.len() as u32 <= STACK_TOP - slot.layout.stack_bytes && m.seed_data(at, &handler),
                "P4 island ISR fits below the dispatcher stack");
        }
        reach(&mut m, slot.symbols.t0_ready, deadline);
        let until = m.now() + duration;
        reach(&mut m, s.started, until);
        let initial = ram_bytes(&m, &state);
        if code == INVALID {
            reach(&mut m, armed, until);
            assert_eq!(m.cpu().ar[10], 1, "P4 corrupts an armed timer");
        }
        if code != 0 {
            loop { reach(&mut m, s.report, until); if m.cpu().ar[2] == u32::from(code) { break; } }
            assert_eq!(m.cpu().ar[3], ets, "P4 reports the ETSTimer used by this gate");
            assert_eq!(ram_word(&m, s.crit_depth), u32::from(code == INVALID && !isr),
                "P4 task handle failure occurs inside the Wi-Fi critical section");
            assert_eq!((r.irq_stack.base..=r.irq_stack.top).contains(&m.cpu().ar[1]), isr,
                "P4 the registered ISR reports on the IRQ stack");
            let tail = ram_word(&m, r.reports + 4);
            assert_eq!(watch.advance(&mut m, until, |m, _| m.pc() == stub), Ran::Pc,
                "P4 the reporting gate stops before returning to the island");
            let at = r.reports + 12 + 12 * (tail % radio::REPORT_RING_ENTRIES);
            assert_eq!((ram_word(&m, r.reports + 4), ram_word(&m, at), ram_word(&m, at + 8), ram_word(&m, s.crit_depth)),
                (tail.wrapping_add(1), u32::from(code), ets, u32::from(code == INVALID && !isr)),
                "P4 publishes the timer report in the original context before quiesce");
            assert_eq!(watch.advance(&mut m, until, |m, _| m.pc() == s.restore), Ran::Pc,
                "P4 no island instruction runs after the reporting gate");
            assert_eq!((ram_word(&m, r.radio_state), ram_word(&m, r.stop_cause), ram_word(&m, r.fault)),
                (radio::RADIO_STOPPED, u32::from(code), 0), "P4 reports stop without creating a fault record");
            assert_eq!(watch.advance(&mut m, until, |m, _| m.pc() == s.started), Ran::Pc,
                "P4 the dispatcher reaches the next marker without running old island code");
            assert_eq!(ram_bytes(&m, &state), initial, "P4 rebuild clears the critical section and its saved mask");
        }
        let mut events = Vec::new(); let mut reports = Vec::new(); let mut starts = Vec::new();
        for mut e in watch_events_until(&mut m, until) {
            match e {
                watch::Event::Fault { code: watch::FAULT_RADIO_STARTED, site, arg } => {
                    assert_eq!(site, watch::SITE_RADIO_SCHEDULER, "P4 start marker site");
                    starts.push(watch::radio_start(arg));
                }
                watch::Event::Fault { code: c, .. } if watch::FAULT_KERNEL_REPORT.contains(&c) => {
                    assert_eq!(starts.len(), 1, "P4 all reports of generation one precede marker two");
                    reports.push(c);
                }
                _ => { p2_check_hello(&mut e, slot); events.push(e); }
            }
        }
        let mut wanted = vec![watch::RadioStart { generation: 1, cause: 0, consecutive: 0 }];
        if code != 0 { wanted.push(watch::RadioStart { generation: 2, cause: code, consecutive: 1 }); }
        assert_eq!(starts, wanted, "P4 exactly the triggering report restarts the radio, even under hold");
        let expected_reports = match code { LOST => vec![UNSET, HEAP, REFUSED, LOST], INVALID => vec![INVALID],
            _ => vec![UNSET, HEAP, REFUSED] };
        assert_eq!(reports, expected_reports, "P4 delivers the old run's timer and allocation outcomes in order");
        assert_eq!(events, expected, "P4 preserves every ordinary app event across the automatic stop");
    }
    // requested_restart_rebuilds_boot_state_and_orders_start_markers covers the full ring.
}

#[test]
fn restart_waits_for_the_last_fault_frame() {
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, enc, reg::*, Asm};
    let island = island_fixture::small_island();
    let data = STACK_TOP - 0x4000;
    let entry = IRAM_START + (data - SRAM_START);
    let image = boot_image(island, "let tick = every(ms(2), fn() {})", BootTask {
        entry, abi: radio::TaskAbi::Windowed, priority: 2, stack_bytes: 2048, args: [0; 6] });
    let r = image.slot.layout.radio.as_ref().unwrap(); let s = image.slot.symbols.radio.as_ref().unwrap();
    let mut fault_pc = 0;
    let worker = assemble(&mut Asm::new(1, A8), |a| {
        a.literal_table(); a.entry(A1, 64);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0);
        a.bcci_reg_label(cc::NE, A4, 1, 0);
        a.mov_reg_i32(A3, r.request); a.movi(A4, 1); a.s32i(A4, A3, 0); a.memw();
        fault_pc = entry + a.base.offset(); a.op24(0);
        a.base.label_assign(0);
        a.mov_reg_i32(A3, r.boot_tasks[0]); a.movi(A4, radio::BLOCKED as i32); a.s32i(A4, A3, radio::TCB_STATE);
        a.movi(A4, 1 << radio::YIELD_LINE); a.wsr(A4, 226); a.rsync();
        a.op24(enc::rrr(0, 0, 0, 6, 0, A3)); a.j(-4);
    });
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(65));
    assert!(image.slot.layout.regions[0].used <= u64::from(data) && data + worker.len() as u32 <= STACK_TOP - image.slot.layout.stack_bytes);
    assert!(m.seed_data(data, &worker));
    let deadline = m.tick_hz();
    reach(&mut m, fault_pc, deadline);
    reach(&mut m, s.restart, deadline);
    assert_eq!(ram_word(&m, r.report_next), 0, "the record waits for marker 1 to leave the ring");
    let ret = m.cpu().ar[0]; reach(&mut m, ret, deadline);
    assert_eq!((ram_word(&m, r.request), ram_word(&m, r.generation)), (1, 1), "dispatcher retains the request");
    reach(&mut m, s.restart, deadline);
    assert!((1..radio::FAULT_FRAMES).contains(&ram_word(&m, r.report_next)));
    assert_eq!((ram_word(&m, r.radio_state), ram_word(&m, r.fault)), (radio::RADIO_DEAD, radio::FAULT_WORDS));
    reach(&mut m, s.started, deadline);
    assert_eq!((ram_word(&m, r.generation), ram_word(&m, r.fault), ram_word(&m, r.stop_cause)), (2, 0, 0));
    let until = m.now() + m.tick_hz() / 20;
    assert_eq!(m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }), Ran::Tick);
    let mut out = Vec::new(); m.take_out(&mut out); m.mark();
    let mut reader = watch::Reader::new(); let mut collector = radio::RecordCollector::new();
    let mut records = 0; let mut restarts = 0;
    for byte in out {
        if let Some(frame) = reader.push(byte.byte) {
            let event = frame.unwrap().event;
            if let Some(record) = collector.push(event).unwrap() {
                assert_eq!(radio::RadioFault::decode(&record).unwrap().pc, fault_pc);
                records += 1;
            }
            if let watch::Event::Fault { code: watch::FAULT_RADIO_STARTED, site: watch::SITE_RADIO_SCHEDULER, arg: 0x0101_0002 } = event {
                assert_eq!(records, 1, "the last old frame reaches the host before the restart marker");
                restarts += 1;
            }
        }
    }
    assert_eq!((records, restarts), (1, 1));
}

#[test]
fn a_second_fault_after_restart_gets_its_own_complete_record() {
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, enc, reg::*, Asm};
    use fab_elec_mcu_xtensa::sim::cause;
    let island = island_fixture::small_island();
    // P6 requests the first recovery and inspects the second fault.
    let radio = Radio { restart: RestartPolicy { hold_after_fault: true, ..RestartPolicy::PROVISIONAL },
        task_capacity: 3, boot_tasks: vec![BootTask { entry: REQUEST_WORKER_ENTRY, abi: radio::TaskAbi::Windowed,
            priority: 2, stack_bytes: 2048, args: [0; 6] }], ..profile(island.binding()) };
    let app = app("let tick = every(ms(2), fn() {})");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let image = build_image(&module, &board(radio), &SlotOptions::default()).unwrap();
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let mut pcs = [0; 2];
    let worker = assemble(&mut Asm::new(1, A8), |a| {
        a.literal_table(); a.entry(A1, 64);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0);
        a.bcci_reg_label(cc::NE, A4, 1, 0);
        pcs[0] = REQUEST_WORKER_ENTRY + a.base.offset(); a.op24(0);
        a.base.label_assign(0); a.movi(A3, 0);
        pcs[1] = REQUEST_WORKER_ENTRY + a.base.offset(); a.op24(enc::rrr(0, 2, 12, A5, A4, A3)); // QUOU by zero
    });
    assert!(slot.layout.regions[0].used <= u64::from(REQUEST_WORKER_DATA)
        && REQUEST_WORKER_DATA + worker.len() as u32 <= slot.layout.stack_top - slot.layout.stack_bytes,
        "P6 worker must fit above allocated SRAM and below the dispatcher stack");
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(65));
    assert!(m.seed_data(REQUEST_WORKER_DATA, &worker), "P6 installs both fault sites before boot");
    let deadline = m.tick_hz();
    let causes = [cause::ILLEGAL, cause::DIVIDE_BY_ZERO];
    let mut captures = Vec::new();
    for (i, pc) in pcs.into_iter().enumerate() {
        reach(&mut m, pc, deadline);
        reach(&mut m, s.fault_entry, deadline);
        assert_eq!((m.cpu().epc1, m.cpu().exccause), (pc, causes[i]), "P6 fault {} reaches the exception vector", i + 1);
        reach(&mut m, s.restore, deadline);
        assert_eq!((ram_word(&m, r.radio_state), ram_word(&m, r.fault)), (radio::RADIO_DEAD, radio::FAULT_WORDS),
            "P6 fault {} publishes its complete record before the dispatcher resumes", i + 1);
        captures.push((0..radio::FAULT_WORDS).map(|n| ram_word(&m, r.fault + 4 * n)).collect::<Vec<_>>());
        if i == 0 {
            // Request after A has drained so TX backpressure cannot conceal the order of B and marker 2.
            let until = m.now() + m.tick_hz() / 15;
            assert_eq!(m.run(&Until { tick: until, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }),
                Ran::Tick, "P6 lets the first complete record reach the listening host");
            assert_eq!(ram_word(&m, r.report_next), radio::FAULT_FRAMES, "P6 finishes sending A before the request");
            assert_eq!(ram_word(&m, slot.layout.tx_head), ram_word(&m, slot.layout.tx_tail), "P6 starts the rebuild with TX drained");
            let epoch = ram_word(&m, slot.layout.link.as_ref().unwrap().epoch);
            send_host_bytes(&mut m, &restart_wire(0, epoch, false));
            reach(&mut m, s.started, deadline);
            assert_eq!((ram_word(&m, r.generation), ram_word(&m, r.fault), ram_word(&m, r.report_next)), (2, 0, 0),
                "P6 restarts with an empty fault record and a fresh frame index");
        }
    }
    let until = m.now() + m.tick_hz() / 15;
    let events = watch_events_until(&mut m, until);
    let mut collector = radio::RecordCollector::new();
    let mut completed = 0;
    let mut index = 0;
    let mut begins = Vec::new(); let mut ends = Vec::new(); let mut markers = Vec::new();
    for (at, event) in events.into_iter().enumerate() {
        match event {
            watch::Event::RadioRecord { index: got, .. } => {
                if completed == 1 {
                    assert!(markers.iter().any(|&(generation, pos)| generation == 2 && pos < at),
                        "P6 generation 2 record frame {got} at event {at} must follow marker 2 on the wire");
                }
                assert_eq!(u32::from(got), index, "P6 frame indices are consecutive within each record");
                if index == 0 { begins.push(at); }
                index += 1;
                if let Some(words) = collector.push(event).expect("P6 frames have consistent lengths, repeats and padding") {
                    assert!(completed < 2, "P6 receives only the two injected faults");
                    assert_eq!(words, captures[completed], "P6 every word came from this generation's published record");
                    let f = radio::RadioFault::decode(&words).expect("P6 host decodes every captured field");
                    assert_eq!((f.pc, f.cause), (pcs[completed], causes[completed]), "P6 host identifies this generation's fault");
                    assert_eq!(index, radio::FAULT_FRAMES, "P6 collector consumed every frame");
                    ends.push(at); completed += 1; index = 0; collector = radio::RecordCollector::new();
                }
            }
            watch::Event::Fault { code: watch::FAULT_RADIO_STARTED, site, arg } => {
                assert_eq!(site, watch::SITE_RADIO_SCHEDULER, "P6 markers identify the scheduler");
                markers.push((watch::radio_start(arg).generation, at));
            }
            watch::Event::Lost { .. } => panic!("P6 lost a watch event"),
            _ => {}
        }
    }
    assert_eq!((completed, index), (2, 0), "P6 receives exactly two whole records with no partial third record");
    assert_eq!(markers.iter().map(|m| m.0).collect::<Vec<_>>(), [1, 2], "P6 only the requested restart publishes a second marker");
    assert_eq!([r.radio_state, r.generation, r.request, r.stop_cause, r.fault, r.report_next].map(|at| ram_word(&m, at)),
        [radio::RADIO_DEAD, 2, 0, 0, radio::FAULT_WORDS, radio::FAULT_FRAMES], "P6 stays DEAD with B published and fully sent");
    assert_eq!([r.reports, r.reports + 4, r.reports + 8].map(|at| ram_word(&m, at)), [2, 2, 0],
        "P6 drains both lifetime start markers without report loss");
    assert_eq!([slot.layout.lost_count, slot.layout.lost_saturated, slot.layout.lost_first, slot.layout.lost_last]
        .map(|at| ram_word(&m, at)), [0; 4], "P6 keeps all watch loss counters clear");
    assert!(ends[0] < markers[1].1 && markers[1].1 < begins[1],
        "P6 marker 2 must separate A's final frame and B's first frame: ends {ends:?}, markers {markers:?}, begins {begins:?}");
}

// The fast host grid puts an OUT packet in EP1 at a particular instruction boundary.
// Outside the injection window, poll once per normal USB frame so WAITI can skip idle time.
fn p2_run(m: &mut S3Machine, stop: Option<u32>, until: u64, fast_until: u64, at: &str) {
    if stop == Some(m.pc()) && m.cpu().will_execute() { return; }
    let stops = stop.as_slice();
    let mut marks = PcSet::default();
    if let Some(pc) = stop { marks = PcSet::new(pc, 1); marks.insert(pc); }
    let cycle = m.tick_hz() / u64::from(fab_elec_mcu_emit_s3::CPU_HZ);
    let frame = s3_fixture::USB_FRAME;
    let start = m.now();
    while m.now() < until {
        let now = m.now();
        let (listen, next) = if now < fast_until { (true, fast_until) } else {
            let next_poll = now - now % frame + frame;
            if now < next_poll - cycle { (false, next_poll - cycle) } else { (true, next_poll) }
        };
        m.set_usb_host_listening(listen);
        match m.run(&Until { tick: until.min(next), marks: &marks, stops, stops_after: start, on_drive: false }) {
            Ran::Tick => {}
            Ran::Pc => return,
            ran => panic!("P2 {at}: {ran:?} at PC {:#x}, {:?}", m.pc(), m.status()),
        }
    }
    assert!(stop.is_none(), "P2 {at}: deadline waiting for {stop:#x?}, PC {:#x}, instret {}", m.pc(), m.cpu().instret);
}

struct ArrivalWorkload {
    sampled: u32,
    gate: [u32; 2],
    critical: [u32; 2],
    isr: [u32; 2],
    switch: [u32; 2],
    fault: [u32; 2],
    illegal: u32,
}

const P2_LOG: u32 = REQUEST_WORKER_DATA - 32;
const P2_SECOND: u32 = REQUEST_WORKER_ENTRY + 0x800;
const P2_ISR: u32 = REQUEST_WORKER_ENTRY + 0xc00;
const P2_USB_LINE: u32 = 1 << 2;

// Task one samples the P1 registers before any workload, then lets the higher-priority
// dispatcher arm its USB wake. GPIO status drives the registered level-one ISR. Pin 5
// selects the ILL in task two only in the fault-window fork.
fn p2_workers(m: &mut S3Machine, slot: &Slot) -> ArrivalWorkload {
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, enc, reg::*, Asm};
    use s3_fixture::{GPIO_IN, GPIO_STATUS, MUX_GPIO, MUX_IE, mux_reg, pin_reg};
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let mut w = ArrivalWorkload { sampled: 0, gate: [0; 2], critical: [0; 2], isr: [P2_ISR; 2],
        switch: [0, P2_SECOND], fault: [0, s.fault_entry], illegal: 0 };
    let store = |a: &mut Asm, addr, value| {
        a.mov_reg_i32(A3, addr); a.mov_reg_i32(A4, value); a.s32i(A4, A3, 0);
    };
    let gate = |a: &mut Asm, name: &str, args: &[u32]| {
        for (i, &arg) in args.iter().enumerate() { a.mov_reg_i32(A10 + i as u8, arg); }
        a.mov_reg_i32(A8, s.thunks.iter().find(|(n, _)| n == name).unwrap().1);
        let call = REQUEST_WORKER_ENTRY + a.base.offset(); a.callx8(A8);
        [call, REQUEST_WORKER_ENTRY + a.base.offset()]
    };
    let block = |a: &mut Asm, tcb| {
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        a.mov_reg_i32(A2, tcb); a.mov_reg_i32(A8, s.block); a.callx0(A8); a.j(-4);
    };
    let first = assemble(&mut Asm::new(2, A8), |a| {
        a.literal_table(); a.entry(A1, 64); a.mov_reg_i32(A5, P2_LOG);
        for (i, reg) in rebuild_registers().into_iter().enumerate() {
            a.mov_reg_i32(A3, reg); a.l32i(A4, A3, 0); a.s32i(A4, A5, 4 * i as u32);
        }
        w.sampled = REQUEST_WORKER_ENTRY + a.base.offset();
        a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0);
        a.bcci_reg_label(cc::EQ, A4, 1, 0); a.j_label(1); a.base.label_assign(0);
        store(a, r.boot_tasks[0] + radio::TCB_PRIORITY, 2);
        a.movi(A4, 1 << radio::YIELD_LINE); a.wsr(A4, 226); a.rsync();
        w.gate = gate(a, "osi_set_intr", &[0, 0, 3, 1]);
        w.critical[0] = gate(a, "osi_wifi_int_disable", &[0])[0];
        a.mov_n(A11, A10);
        w.critical[1] = gate(a, "osi_wifi_int_restore", &[0])[1];
        gate(a, "osi_set_isr", &[3, P2_ISR, 0]); gate(a, "osi_ints_on", &[1 << 3]);
        store(a, mux_reg(5), MUX_GPIO | MUX_IE);
        store(a, GPIO_STATUS + 8, 1 << 4); store(a, pin_reg(4), (1 << 7) | (1 << 13));
        store(a, radio::kernel::INTR_MATRIX + 16 * 4, 3);
        store(a, GPIO_STATUS + 4, 1 << 4);
        a.base.label_assign(1);
        w.switch[0] = REQUEST_WORKER_ENTRY + a.base.offset();
        block(a, r.boot_tasks[0]);
    });
    let second = assemble(&mut Asm::new(1, A8), |a| {
        a.literal_table(); a.entry(A1, 64);
        a.mov_reg_i32(A3, r.generation); a.l32i(A4, A3, 0); a.bcci_reg_label(cc::NE, A4, 1, 0);
        a.mov_reg_i32(A3, GPIO_IN); a.l32i(A4, A3, 0); a.extui(A4, A4, 5, 1);
        a.bccz_reg_label(ccz::EQ, A4, 0);
        a.op24(enc::rrr(0, 0, 0, 6, 15, A3));
        w.fault[0] = P2_SECOND + a.base.offset();
        a.movi(A4, 1); a.addi(A4, A4, 1); a.memw();
        w.illegal = P2_SECOND + a.base.offset(); a.op24(0);
        a.base.label_assign(0); block(a, r.boot_tasks[1]);
    });
    let handler = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table(); a.entry(A1, 64);
        store(a, GPIO_STATUS + 8, 1 << 4);
        w.isr[1] = P2_ISR + a.base.offset(); a.retw_n();
    });
    assert!(slot.layout.regions[0].used <= u64::from(P2_LOG), "P2 log must be above the allocated radio RAM");
    for (offset, code, limit) in [(0, &first, REQUEST_WORKER_DATA + 0x800),
        (0x800, &second, REQUEST_WORKER_DATA + 0xc00), (0xc00, &handler, STACK_TOP - slot.layout.stack_bytes)] {
        let data = REQUEST_WORKER_DATA + offset;
        assert!(data + code.len() as u32 <= limit && m.seed_data(data, code),
            "P2 fixture code at {data:#x} must fit before boot");
    }
    w
}

fn p2_check_hello(event: &mut watch::Event, slot: &Slot) {
    if let watch::Event::Hello { image, .. } = event {
        assert_eq!(*image, slot.image_id, "P2 a Hello identifies its own slot");
        *image = 0;
    }
}

fn p2_check_stream(events: Vec<watch::Event>, slot: &Slot, expected: &[watch::Event], last_generation: u32,
    capture: Option<&[u32]>, illegal: u32, at: &str)
{
    let mut app_events = Vec::new(); let mut markers = Vec::new();
    let mut frames = 0; let mut collector = radio::RecordCollector::new(); let mut complete = false;
    let mut later_writes = 0;
    for mut event in events {
        match event {
            watch::Event::Fault { code: watch::FAULT_RADIO_STARTED, site, arg } => {
                assert_eq!(site, watch::SITE_RADIO_SCHEDULER, "P2 {at}: marker site");
                markers.push(watch::radio_start(arg));
            }
            watch::Event::RadioRecord { index, .. } => {
                assert!(capture.is_some() && markers.len() == 1, "P2 {at}: fault frames belong to generation one");
                assert_eq!(u32::from(index), frames, "P2 {at}: fault frames are complete and ordered");
                frames += 1;
                if let Some(words) = collector.push(event).unwrap_or_else(|e| panic!("P2 {at}: invalid record frame {index}: {e}")) {
                    assert_eq!(Some(words.as_slice()), capture, "P2 {at}: every word matches the record at publication");
                    let f = radio::RadioFault::decode(&words).unwrap_or_else(|e| panic!("P2 {at}: incomplete host record: {e}"));
                    assert_eq!((f.pc, f.cause), (illegal, fab_elec_mcu_xtensa::sim::cause::ILLEGAL), "P2 {at}: host identifies the injected fault");
                    complete = true;
                }
            }
            watch::Event::Fault { code, site: watch::SITE_RADIO_SCHEDULER, .. }
                if watch::FAULT_KERNEL_REPORT.contains(&code) => {}
            _ => {
                if markers.len() == last_generation as usize && matches!(event, watch::Event::Write { .. }) { later_writes += 1; }
                p2_check_hello(&mut event, slot); app_events.push(event);
            }
        }
    }
    assert_eq!((complete, frames), (capture.is_some(), if capture.is_some() { radio::FAULT_FRAMES } else { 0 }),
        "P2 {at}: pending restart preserves exactly the expected record");
    let starts: Vec<_> = (1..=last_generation).map(|g| watch::RadioStart { generation: g as u16,
        cause: u8::from(g > 1), consecutive: (g - 1) as u8 }).collect();
    assert_eq!(markers, starts, "P2 {at}: exactly one ordered start marker per wire command");
    assert_eq!(app_events, expected, "P2 {at}: the complete app stream matches the radio-off control");
    assert!(later_writes >= 2, "P2 {at}: both app writes continue after the final marker");
}

#[test]
fn a_restart_request_at_any_instruction_yields_a_marker_and_a_clean_rebuild() {
    use std::collections::BTreeSet;
    let island = island_fixture::small_island();
    let app = pin_app("let a = output(D33)\nlet b = output(D32)\nvar n: i32 = 0\n\
        let tick = every(ms(2), fn() { n += 1\n write(a, n & 1)\n write(b, (n + 1) & 1) })");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let options = SlotOptions { sites: app.sites.iter().map(|s| (s.line, s.col)).collect(), ..SlotOptions::default() };
    let mut radio = profile(island.binding());
    radio.task_capacity = 4; radio.dispatcher_priority = 3; radio.irq_stack_bytes = 2048;
    for (entry, priority) in [(REQUEST_WORKER_ENTRY, 4), (P2_SECOND, 1)] {
        radio.boot_tasks.push(BootTask { entry, priority, abi: radio::TaskAbi::Windowed, stack_bytes: 2048, args: [0; 6] });
    }
    let mut board = board(radio);
    let image = build_image(&module, &board, &options).unwrap();
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let s = slot.symbols.radio.as_ref().unwrap();
    let mut profile = s3_fixture::profile(79);
    let ms = s3_fixture::USB_FRAME;
    let cycle = ms * 1000 / u64::from(fab_elec_mcu_emit_s3::CPU_HZ);
    profile.pins.usb_service_ticks = cycle;
    let mut root = island_fixture::machine_with_island(&image.flash, &island.image, &profile);
    let w = p2_workers(&mut root, slot);
    root.set_input(5, false);
    root.set_usb_host_listening(false);
    let boot_deadline = root.tick_hz();
    reach(&mut root, slot.symbols.t0_ready, boot_deadline);
    let t0 = root.now();
    root.set_usb_host_listening(true);
    reach(&mut root, s.started, boot_deadline);
    let ranges = rebuild_ranges(slot, island.window_end);
    let boot: Vec<_> = ranges.iter().map(|span| ram_bytes(&root, span)).collect();
    let lines = !(runtime::DISPATCHER_LINES | (1 << radio::YIELD_LINE));
    let boot_lines = root.cpu().intenable & lines;
    let check_marker = |m: &S3Machine, generation: u32, at: &str| {
        assert_eq!((m.pc(), ram_word(m, r.generation), ram_word(m, r.consecutive)), (s.started, generation, generation - 1),
            "P2 {at}: each accepted request advances the generation once");
        assert_eq!((ram_word(m, r.island_status), m.cpu().ps & 15, m.cpu().intenable & lines),
            (radio::ISLAND_READY, 15, boot_lines), "P2 {at}: READY publication restores the boot interrupt state");
        for (span, original) in ranges.iter().zip(&boot) {
            let rebuilt = ram_bytes(m, span);
            if let Some(i) = original.iter().zip(&rebuilt).position(|(a, b)| a != b) {
                panic!("P2 {at}: generation {generation}, {:#010x} was {:#04x} at boot, got {:#04x}",
                    span.start + i as u32, original[i], rebuilt[i]);
            }
        }
    };
    check_marker(&root, 1, "boot");
    reach(&mut root, w.sampled, boot_deadline);
    let registers = |m: &S3Machine| -> [u32; 8] { std::array::from_fn(|i| ram_word(m, P2_LOG + i as u32 * 4)) };
    let boot_regs = registers(&root);
    let gate = s.gate_reports.iter().find(|g| g.name == "osi_set_intr").unwrap().address;

    // End both controls 1.75 ms after a firing: a full USB frame to deliver its writes,
    // before the next 2 ms period. Allow a sleep turn per pair of fault frames.
    let short = 8 * ms - ms / 4;
    let long = short + (2 * u64::from(radio::FAULT_FRAMES.div_ceil(2)) + 2) * ms;
    board.radio = None;
    let off = build_image(&module, &board, &options).unwrap();
    let mut control = island_fixture::machine_with_island(&off.flash, &[], &s3_fixture::profile(79));
    reach(&mut control, off.slot.symbols.t0_ready, boot_deadline);
    let off_t0 = control.now();
    let mut total_retired = root.cpu().instret + control.cpu().instret;
    let expected = [short, long].map(|duration| {
        let mut replay = control.clone();
        let mut events = watch_events_until(&mut replay, off_t0 + duration);
        total_retired += replay.cpu().instret - control.cpu().instret;
        for event in &mut events { p2_check_hello(event, &off.slot); }
        assert_eq!(events.iter().filter(|e| matches!(e, watch::Event::Write { .. })).count() as u64,
            2 * (duration / (2 * ms)), "P2 radio-off control observes every pair of timer writes");
        events
    });

    for name in ["gate", "critical", "ISR", "switch", "restart", "fault"] {
        let nested = name == "restart";
        let fault = name == "fault";
        let finish = t0 + if fault { long } else { short };
        let mut base = root.clone();
        let mut retired = 0;
        let epoch = ram_word(&base, slot.layout.link.as_ref().unwrap().epoch);
        let wire = restart_wire(u8::from(nested), epoch, false);
        let [begin, end] = match name {
            "gate" => w.gate, "critical" => w.critical, "ISR" => w.isr, "switch" => w.switch,
            "fault" => { base.set_input(5, true); w.fault }
            _ => {
                reach(&mut base, s.idle, finish);
                send_host_bytes(&mut base, &restart_wire(0, epoch, false));
                let mut first = base.clone(); reach(&mut first, s.restart, finish);
                retired += first.cpu().instret - base.cpu().instret;
                assert_eq!(ram_word(&first, r.request), 1, "P2 nested window starts on an accepted wire request");
                [s.restart, first.cpu().ar[0]]
            }
        };

        // First trace the uninterrupted window. A host poll on the last cycle of the
        // preceding instruction makes RX pending at exactly this (PC, instret), before
        // the CPU can take the interrupt. The ILL itself retires zero instructions.
        let mut trace = base.clone();
        reach(&mut trace, begin, finish);
        let first_retired = trace.cpu().instret;
        if name == "ISR" {
            assert_eq!((ram_word(&trace, r.isr_count), ram_word(&trace, r.isrs), ram_word(&trace, r.isrs + 4)),
                (1, 1 << 3, P2_ISR), "P2 ISR is the island handler registered on line 3");
            assert!(trace.cpu().ar[1] >= r.irq_stack.base && trace.cpu().ar[1] <= r.irq_stack.top,
                "P2 ISR runs on the interrupt stack");
        }
        if name == "switch" {
            assert_eq!(ram_word(&trace, r.current), r.boot_tasks[0], "P2 switch starts in the first island task");
        }
        let mut cases = Vec::new();
        let mut edges = BTreeSet::new();
        let mut previous = None;
        let mut first_marker = None;
        let mut held = 0;
        let mut entered_gate = false;
        loop {
            let pc = trace.pc();
            entered_gate |= pc == gate;
            if pc == s.started { check_marker(&trace, 2, "uninterrupted first rebuild"); first_marker = Some(trace.now()); }
            if name == "critical" && ram_word(&trace, s.crit_depth) != 0 { held += 1; }
            // Replaying every trip of the word-copy/clear loops would be quadratic.
            // Cover every executed instruction edge, including loop back-edges, exits,
            // feed trips and both sides of the first publication, in the long masked window.
            if !nested || edges.insert((previous, pc)) {
                assert_eq!(trace.cpu().interrupt & P2_USB_LINE, 0, "P2 {name}: undisturbed USB line is quiet at {pc:#x}, instret {}",
                    trace.cpu().instret);
                cases.push((trace.now(), pc, trace.cpu().instret));
            }
            if pc == end { break; }
            previous = Some(pc);
            let tick = trace.now() + cycle;
            assert!(tick < finish, "P2 {name} trace hung at {pc:#x}, instret {}", trace.cpu().instret);
            p2_run(&mut trace, None, tick, finish, name);
        }
        if nested { assert!(first_marker.is_some(), "P2 restart window includes its first start marker"); }
        if name == "gate" { assert!(entered_gate, "P2 follows the island thunk into the compiled osi_set_intr gate"); }
        if name == "critical" { assert!(held > 0, "P2 exercised instructions inside the held critical section"); }
        if name == "switch" {
            assert_eq!((ram_word(&trace, r.boot_tasks[0] + radio::TCB_STATE), ram_word(&trace, r.current)),
                (radio::BLOCKED, r.boot_tasks[1]), "P2 switch blocks the first island task and restores the second");
        }
        let fast_until = trace.now() + 2 * cycle;
        let window = trace.cpu().instret - first_retired + 1;
        retired += trace.cpu().instret - root.cpu().instret;
        let mut witness: Option<(S3Machine, bool)> = None;
        let mut replays = 0;
        for &(tick, pc, instret) in &cases {
            let at = format!("{name} PC {pc:#010x} instret {instret}");
            let before = base.cpu().instret;
            p2_run(&mut base, None, tick - cycle, fast_until, &at);
            retired += base.cpu().instret - before;
            assert_eq!(base.now(), tick - cycle, "P2 {at}: prepare exactly one CPU cycle before arrival");
            let mut m = base.clone();
            assert_eq!(m.cpu().interrupt & P2_USB_LINE, 0, "P2 {at}: USB line must be quiet before OUT");
            send_host_bytes(&mut m, &wire);
            p2_run(&mut m, None, tick, fast_until, &at);
            retired += m.cpu().instret - base.cpu().instret;
            assert_eq!((m.pc(), m.cpu().instret, m.cpu().interrupt & P2_USB_LINE), (pc, instret, P2_USB_LINE),
                "P2 {at}: the complete frame reached EP1 at the chosen instruction");
            let in_critical = name == "critical" && ram_word(&m, s.crit_depth) != 0;
            if in_critical {
                assert!(m.cpu().interrupt_level() >= radio::kernel::CRIT_MASK as u8 && ram_word(&m, r.request) == 0,
                    "P2 {at}: the USB request waits while the critical section masks it");
            }
            // The next poll replaces the USB block's last-event timestamp and bits.
            // If two injection times now give the same whole machine, USB output included,
            // the already checked suffix proves this fork too. Otherwise run it in full.
            let mut next = m.clone();
            p2_run(&mut next, None, tick + cycle, fast_until, &at);
            retired += next.cpu().instret - instret;
            if let Some((old, was_critical)) = &mut witness {
                let from = old.cpu().instret;
                p2_run(old, None, next.now(), fast_until, &at);
                retired += old.cpu().instret - from;
                if *was_critical == in_critical && next.same_state(old).is_none() { continue; }
            }
            witness = Some((next, in_critical));
            replays += 1;
            let capture = if fault {
                p2_run(&mut m, Some(s.restore), finish, fast_until, &at);
                assert_eq!(ram_word(&m, r.fault), radio::FAULT_WORDS, "P2 {at}: fault is published before the resumed pick");
                Some((0..radio::FAULT_WORDS).map(|i| ram_word(&m, r.fault + 4 * i)).collect::<Vec<_>>())
            } else { None };
            if in_critical {
                p2_run(&mut m, Some(s.restart), finish, fast_until, &at);
                assert_eq!((ram_word(&m, s.crit_depth), ram_word(&m, r.generation)), (0, 1),
                    "P2 {at}: dispatcher takes the request only after the critical section exits");
            }
            let last_generation = if nested { 3 } else { 2 };
            for generation in 2..=last_generation {
                // Forks after the first publication share its already checked prefix.
                if generation != 2 || !first_marker.is_some_and(|first| first < tick) {
                    p2_run(&mut m, Some(s.started), finish, fast_until, &at);
                    check_marker(&m, generation, &at);
                }
                p2_run(&mut m, Some(w.sampled), finish, fast_until, &at);
                assert_eq!(registers(&m), boot_regs, "P2 {at}: generation {generation} restores the eight P1 registers");
            }
            p2_run(&mut m, None, finish, fast_until, &at);
            retired += m.cpu().instret - instret;
            p2_check_stream(watch_events(&mut m, &at), slot, &expected[usize::from(fault)], last_generation,
                capture.as_deref(), w.illegal, &at);
        }
        eprintln!("P2 {name}: {} injections across {window} instruction boundaries, {replays} full replays, {retired} retired",
            cases.len());
        total_retired += retired;
    }
    eprintln!("P2 total including boot and the radio-off control: {total_retired} retired instructions");
}

#[test]
fn ready_loads_segments_gates_guard_and_calls_fixture() {
    use fab_elec_mcu_emit32::xtensa::{assemble, Asm};
    let data = (0..65539u32).map(|n| (n ^ (n >> 8) ^ (n >> 16)) as u8).collect();
    let island = island_fixture::linked_island(data, 1025);
    let entry = island.entries[1];
    let driver_data = STACK_TOP - 0x4000;
    let driver = IRAM_START + (driver_data - SRAM_START);
    let image = boot_image(&island, "fn setup() {}", BootTask { entry: driver, abi: radio::TaskAbi::Call0,
        priority: 2, stack_bytes: 1024, args: [entry, 0, 0, 0, 0, 0] });
    let slot = &image.slot;
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let r = slot.layout.radio.as_ref().unwrap();
    // Reproduce a handoff with Wi-Fi forced off and isolated. Seed unrelated SYSCON
    // bits as well so a whole-register write of a reset-based value is observable.
    let initial = [0x0054_5010 | (1 << 17), 0xaa80_5080 | (1 << 28), 0x8200_0040, 0x4000_8000];
    let mut p = s3_fixture::profile(31);
    let stage = p.entry;
    let setup = assemble(&mut Asm::new(0, 3), |a| {
        a.literal_table();
        for (address, value) in WIFI_POWER_REGS.into_iter().zip(initial) {
            a.mov_reg_i32(2, address); a.mov_reg_i32(3, value); a.s32i(3, 2, 0);
        }
        a.mov_reg_i32(2, stage); a.jx(2);
    });
    p.entry = IRAM_START + (slot.layout.t0 - SRAM_START);
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &p);
    assert!(setup.len() <= slot.layout.runtime_bytes as usize);
    assert!(m.seed_data(slot.layout.t0, &setup));
    let heap = r.heap..r.heap + FIXTURE_HEAP_BYTES;
    assert!(r.start <= heap.start && heap.end <= r.end,
        "boot's cleared radio allocation must cover the whole heap for the objects.splash zero-heap contract");
    assert!(r.clear_spans().iter().any(|span| span.start <= heap.start && heap.end <= span.end),
        "one restart clear span must cover the whole heap for the objects.splash zero-heap contract");
    assert!(m.seed_data(heap.start, &vec![0xa5; FIXTURE_HEAP_BYTES as usize]),
        "boot must begin with dirty backing bytes to exercise the objects.splash zero-heap contract");
    let mut returned = 0;
    let caller = assemble(&mut Asm::new(0, 8), |a| {
        a.literal_table(); a.mov_reg_i32(8, symbols.shim); a.callx0(8);
        returned = driver + a.base.offset(); a.j(-4);
    });
    assert!(slot.layout.regions[0].used <= u64::from(driver_data)
        && driver_data + caller.len() as u32 <= STACK_TOP - slot.layout.stack_bytes);
    assert!(m.seed_data(driver_data, &caller));
    let window = (island.window_end - SRAM_START) as usize;
    let mut expected = vec![0xa5; window];
    assert!(m.seed_data(SRAM_START, &expected));
    assert!(m.seed_data(symbols.island_status, &u32::MAX.to_le_bytes()));
    assert!(island.segments.iter().any(|s| s.kind == SegmentKind::Data && s.file_offset < 65536 && s.file_offset + s.bytes > 65536));
    for s in &island.segments {
        let offset = match s.kind {
            SegmentKind::Iram => s.address - IRAM_START,
            SegmentKind::Data | SegmentKind::Bss => s.address - SRAM_START,
            _ => continue,
        } as usize;
        let dst = &mut expected[offset..offset + s.bytes as usize];
        if s.kind == SegmentKind::Bss { dst.fill(0); }
        else { dst.copy_from_slice(&island.image[s.file_offset as usize..][..s.bytes as usize]); }
    }
    for (index, (_, thunk)) in symbols.island_thunks.iter().enumerate() {
        expected[index * 4..index * 4 + 4].copy_from_slice(&thunk.to_le_bytes());
    }
    expected[(island.guard - SRAM_START) as usize..].copy_from_slice(&radio::GUARD.to_le_bytes());

    let deadline = m.tick_hz();
    reach(&mut m, stage, deadline);
    let watch = StoreWatch::new(&image, &[runtime::INIT, symbols.pick], vec![]);
    let mut last_feed = None;
    let mut power_stores = Vec::new();
    let mut delay_feeds = Vec::new();
    let mut longest = 0;
    let mut first_runtime_gap = None;
    let mut init = None;
    let mut pages = 0;
    let ran = watch.advance(&mut m, deadline, |m, store| {
        assert_ne!(m.pc(), symbols.pick, "READY precedes the first pick");
        if m.pc() == runtime::INIT { init = Some((m.now(), m.cpu().instret)); }
        let Some((address, width)) = store else { return false };
        if WIFI_POWER_REGS.contains(&address) {
            assert!(m.pc() >= SLOT_BASE, "power sequence belongs to masked slot init");
            assert_eq!(width, 4, "word store to {address:#010x}");
            let i = insn(&slot.bytes[(m.pc() - SLOT_BASE) as usize..]).unwrap();
            power_stores.push((address, m.cpu().ar[i.t as usize], m.now()));
        }
        if address == 0x6000_80ac {
            if power_stores.len() == 1 { delay_feeds.push(m.now()); }
            if let Some(last) = last_feed.replace(m.now()) {
                let gap = m.now() - last;
                if first_runtime_gap.is_none() && m.pc() >= SLOT_BASE { first_runtime_gap = Some(gap); }
                longest = longest.max(gap);
            }
            else { assert!(m.pc() < SLOT_BASE, "first feed comes from the stage"); }
        }
        if m.pc() >= SLOT_BASE && (0x600c_5000..0x600c_5800).contains(&address) {
            assert_eq!(address, 0x600c_5000 + (ISLAND_MMU_PAGE + pages) * 4);
            let i = insn(&slot.bytes[(m.pc() - SLOT_BASE) as usize..]).unwrap();
            assert_eq!(m.cpu().ar[i.t as usize], VOICEBOX.island / 65536 + pages);
            pages += 1;
        }
        if address != symbols.island_status { return false; }
        let i = insn(&slot.bytes[(m.pc() - SLOT_BASE) as usize..]).unwrap();
        m.cpu().ar[i.t as usize] == radio::ISLAND_READY
    });
    assert_eq!(ran, Ran::Pc, "masked init publishes READY");
    // ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:427–447. The delay between
    // power-up and clock enable is the second step, with one watchdog feed.
    let [pwc, iso, clk, rst] = WIFI_POWER_REGS;
    let common = 0x0078_078f;
    let modem = 0x0000_2a1f;
    let expected_stores = [
        (pwc, initial[0] & !(1 << 17)),
        (clk, initial[2] | common),
        (rst, initial[3] | modem),
        (rst, initial[3] & !modem),
        (iso, initial[1] & !(1 << 28)),
        (clk, initial[2] & !common),
    ];
    assert_eq!(power_stores.len(), expected_stores.len(), "six domain-register writes precede READY");
    for (&(address, value, tick), expected) in power_stores.iter().zip(expected_stores) {
        assert_eq!((address, value), expected, "power sequence preserves unrelated bits at tick {tick}");
        assert!(tick < m.now(), "domain write precedes READY publication");
    }
    let pd_tick = power_stores[0].2;
    let clk_tick = power_stores[1].2;
    assert!(delay_feeds.len() == POWER_ON_STRETCHES && delay_feeds.iter().all(|&tick| pd_tick < tick && tick < clk_tick),
        "one watchdog feed before each power-up stretch, between power-up and clock enable");
    assert!(clk_tick - pd_tick >= m.tick_hz() / 100_000, "Wi-Fi settles for at least 10 us");
    assert_eq!(m.cpu().ps & 15, 15);
    assert_eq!(ram_word(&m, symbols.island_status), radio::ISLAND_NOT_LOADED);
    assert_eq!(m.run(&Until { tick: m.now() + 1, marks: &PcSet::default(), stops: &[], stops_after: 0, on_drive: false }), Ran::Tick);
    assert_eq!(ram_word(&m, symbols.island_status), radio::ISLAND_READY);
    assert!(ram_bytes(&m, &heap).iter().all(|&b| b == 0),
        "boot READY must publish the zeroed heap required by objects.splash before the island runs");
    assert_eq!(ram_word(&m, r.task_count), 0, "no task has been installed at publication");
    assert_eq!(pages, VOICEBOX.island_pages);
    longest = longest.max(m.now() - last_feed.expect("stage feed"));
    let (hz, denom) = p.time.rc_slow_hz;
    let hold_ticks = ((u64::from(image.record.wdt) << (1 + p.time.wdt_delay_sel)) * denom * m.tick_hz()) / hz;
    assert!(longest < hold_ticks, "every feed gap through READY fits the recorded hold");
    let (tick, retired) = init.unwrap();
    eprintln!("INIT to READY: {} ticks, {} retired instructions; stage to first runtime feed: {} ticks; largest gap: {longest} of {hold_ticks} ticks",
        m.now() - tick, m.cpu().instret - retired, first_runtime_gap.expect("first runtime feed"));
    let mut actual = vec![0; window];
    assert!(m.read_data(SRAM_START, &mut actual));
    if let Some(i) = actual.iter().zip(&expected).position(|(a, b)| a != b) {
        panic!("island byte at {:#010x}: loaded {:#04x}, expected {:#04x}", SRAM_START + i as u32, actual[i], expected[i]);
    }
    for (i, (_, row)) in radio::adapter::OSI_FIELDS.iter().enumerate() {
        if let radio::adapter::Entry::Glue(name) = row {
            assert_eq!(ram_word(&m, r.osi_table + i as u32 * 4), island.glue[*name], "callable {name} entry");
        }
    }
    reach(&mut m, symbols.pick, deadline);
    assert_eq!(ram_word(&m, r.task_count), 3, "READY installs the fixture task before the first pick");
    for pc in [symbols.shim, entry, returned] { reach(&mut m, pc, deadline); }
    assert_eq!(m.cpu().ar[2], 1, "the real shim returns the second fixture entry's result");
}

#[test]
fn island_start_restores_memory_and_power_on_a_second_call() {
    use fab_elec_mcu_emit32::xtensa::{assemble, cc, enc, Asm};

    // End DATA on a BSS alignment boundary so the whole DATA..BSS span is loadable.
    let island = island_fixture::linked_island((0..16).collect(), 17);
    let data = island.segments.iter().find(|s| s.kind == SegmentKind::Data).unwrap();
    let bss = island.segments.iter().find(|s| s.kind == SegmentKind::Bss).unwrap();
    assert_eq!(data.address + data.bytes, bss.address);
    let driver_data = STACK_TOP - 0x4000;
    let driver = IRAM_START + (driver_data - SRAM_START);
    let image = boot_image(&island, "fn setup() {}", BootTask { entry: driver, abi: radio::TaskAbi::Call0,
        priority: 2, stack_bytes: 1024, args: [0; 6] });
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let start = slot.map.runtime.iter().find(|routine| routine.name == "island_start").unwrap().start;
    let state = (r.kernel_state, r.kernel_state_words * 4);
    let osi = (r.osi_table, radio::adapter::OSI_FIELDS.len() as u32 * 4);
    let dirty = [(data.address, data.bytes + bss.bytes), state, osi];
    let initial = [0x0054_5010 | (1 << 17), 0xaa80_5080 | (1 << 28), 0x8200_0040, 0x4000_8000];
    let mut halt = 0;
    let caller = assemble(&mut Asm::new(dirty.len(), 8), |a| {
        a.literal_table();
        a.op24(enc::rrr(0, 0, 0, 6, 15, 2)); // RSIL a2,15
        a.movi(4, 0xa5);
        for (label, (base, len)) in dirty.into_iter().enumerate() {
            a.mov_reg_i32(2, base); a.mov_reg_i32(3, base + len);
            a.base.label_assign(label);
            a.s8i(4, 2, 0); a.addi(2, 2, 1); a.bcc_reg_reg_label(cc::NE, 2, 3, label);
        }
        a.mov_reg_i32(2, r.island_status); a.movi(3, 0); a.s32i(3, 2, 0);
        for (address, value) in WIFI_POWER_REGS.into_iter().zip(initial) {
            a.mov_reg_i32(2, address); a.mov_reg_i32(3, value); a.s32i(3, 2, 0);
        }
        a.mov_reg_i32(8, start); a.callx0(8);
        halt = driver + a.base.offset(); a.op24(0x004000); // BREAK at the CALL0 return address.
    });
    assert!(slot.layout.regions[0].used <= u64::from(driver_data)
        && driver_data + caller.len() as u32 <= STACK_TOP - slot.layout.stack_bytes);
    let mut m = island_fixture::machine_with_island(&image.flash, &island.image, &s3_fixture::profile(31));
    assert!(m.seed_data(driver_data, &caller));
    let deadline = m.tick_hz();
    reach(&mut m, start, deadline);
    let boot_return = m.cpu().ar[0];
    reach(&mut m, boot_return, deadline);
    assert_eq!(ram_word(&m, r.island_status), radio::ISLAND_READY);
    let read = |m: &S3Machine, (base, len): (u32, u32)| {
        let mut bytes = vec![0; len as usize];
        assert!(m.read_data(base, &mut bytes), "read {len} bytes at {base:#010x}");
        bytes
    };
    let spans = [(SRAM_START, island.window_end - SRAM_START), state, osi];
    let first = spans.map(|span| read(&m, span));

    reach(&mut m, start, deadline);
    assert_eq!(m.cpu().ar[0], halt);
    let ps = m.cpu().ps;
    assert_eq!(ps & 15, 15);
    assert_eq!(ram_word(&m, r.island_status), radio::ISLAND_NOT_LOADED);
    for span in dirty {
        assert!(read(&m, span).iter().all(|&b| b == 0xa5), "caller dirtied the range at {:#010x}", span.0);
    }
    let watch = StoreWatch::new(&image, &[], vec![]);
    let mut stores = Vec::new();
    let mut delay_feeds = Vec::new();
    let ran = watch.advance(&mut m, deadline, |m, store| {
        assert_eq!(m.cpu().ps & 15, 15, "second start stays masked");
        if let Some((address, width)) = store {
            if WIFI_POWER_REGS.contains(&address) {
                assert_eq!(width, 4);
                let i = insn(&slot.bytes[(m.pc() - SLOT_BASE) as usize..]).unwrap();
                stores.push((address, m.cpu().ar[i.t as usize], m.now()));
            } else if address == 0x6000_80ac && stores.len() == 1 {
                delay_feeds.push(m.now());
            }
        }
        false
    });
    assert_eq!(ran, Ran::Halted, "second CALL0 returns to BREAK");
    assert!(matches!(m.status(), Status::Halted(h) if h.kind == HaltKind::Break && h.pc == halt), "{:?}", m.status());
    assert_eq!(m.cpu().ps, ps);
    assert_eq!(ram_word(&m, r.island_status), radio::ISLAND_READY);
    for (span, expected) in spans.into_iter().zip(first) {
        assert_eq!(read(&m, span), expected, "second start restores the range at {:#010x}", span.0);
    }

    // ESP-IDF v6.1.1 components/esp_phy/src/phy_init.c:427–447.
    let [pwc, iso, clk, rst] = WIFI_POWER_REGS;
    let common = 0x0078_078f;
    let modem = 0x0000_2a1f;
    let expected = [(pwc, initial[0] & !(1 << 17)), (clk, initial[2] | common),
        (rst, initial[3] | modem), (rst, initial[3] & !modem),
        (iso, initial[1] & !(1 << 28)), (clk, initial[2] & !common)];
    assert_eq!(stores.iter().map(|&(addr, value, _)| (addr, value)).collect::<Vec<_>>(), expected);
    let (pd_tick, clk_tick) = (stores[0].2, stores[1].2);
    assert!(clk_tick - pd_tick >= m.tick_hz() / 100_000, "second power-up waits at least 10 us");
    assert!(delay_feeds.len() == POWER_ON_STRETCHES && delay_feeds.iter().all(|&tick| pd_tick < tick && tick < clk_tick),
        "one watchdog feed before each stretch of the second power-up delay");
}

fn failure_image() -> &'static Image {
    static IMAGE: std::sync::OnceLock<Image> = std::sync::OnceLock::new();
    IMAGE.get_or_init(|| {
        let island = island_fixture::small_island();
        boot_image(island, "
var progress: i32 = 0
let packet = bytes(1514)
fn setup() { progress = 1 }
let timer = after(ms(2), fn() { progress += 2 })
let network = eth(1, fn() {
    let n = eth_recv(packet, 0)
    if n > 0 { eth_send(packet, 0, n) }
})", BootTask { entry: island.entries[1], abi: radio::TaskAbi::Windowed, priority: 2,
            stack_bytes: 1024, args: [0; 6] })
    })
}

fn check_failed_boot(flash_island: &[u8], status: u32, crc: u32) {
    let image = failure_image();
    let island = island_fixture::small_island();
    let slot = &image.slot;
    let r = slot.layout.radio.as_ref().unwrap();
    let symbols = slot.symbols.radio.as_ref().unwrap();
    let p = s3_fixture::profile(47);
    let mut m = island_fixture::machine_with_island(&image.flash, flash_island, &p);
    let size = island.window_end - SRAM_START;
    let xip = s3_fixture::FLASH + ISLAND_MMU_PAGE * 65536;
    let window = [SRAM_START..island.window_end, IRAM_START..IRAM_START + size];
    let forbidden = vec![window[0].clone(), window[1].clone(), xip..xip + island.header.image_size];
    let poison = vec![0x6d; size as usize];
    assert!(m.seed_data(SRAM_START, &poison));
    let stores = StoreWatch::new(image, &[symbols.idle], forbidden);
    let check = |pc, store: Option<(u32, u32)>| {
        if let Some((addr, width)) = store {
            assert!(!WIFI_POWER_REGS.contains(&addr), "failed boot wrote a Wi-Fi domain register {addr:#010x} at {pc:#010x}");
            for r in &window {
                assert!(u64::from(addr) + u64::from(width) <= u64::from(r.start) || addr >= r.end,
                    "store into protected window at {addr:#010x} from {pc:#010x}");
            }
        }
    };
    let deadline = m.tick_hz();
    assert_eq!(stores.advance(&mut m, deadline, |m, store| {
        check(m.pc(), store); m.pc() == symbols.idle
    }), Ran::Pc, "dispatcher reaches idle after boot");
    assert_eq!(ram_word(&m, symbols.island_status), status);
    assert_eq!(ram_word(&m, r.radio_state), radio::RADIO_RUNNING);
    assert_eq!(ram_word(&m, r.task_count), 2);
    assert_eq!([0, 4, 8].map(|off| ram_word(&m, r.tasks + off)), [r.dispatcher, r.idle, 0]);
    let mut tcb = vec![0; radio::TCB_SIZE as usize];
    assert!(m.read_data(r.boot_tasks[0], &mut tcb) && tcb.iter().all(|&b| b == 0), "no island TCB installation");
    let mut stack = vec![0; (r.boot_stacks[0].top - r.boot_stacks[0].base) as usize];
    assert!(m.read_data(r.boot_stacks[0].base, &mut stack) && stack.iter().all(|&b| b == 0), "no island entry frame");

    // Let the host receive Hello, its initial credit and the boot report, then echo a real USB link frame.
    let interval = m.tick_hz() / 100;
    let mut reader = watch::Reader::new();
    let mut reports = Vec::new();
    let mut epoch = None;
    let mut echoes = Vec::new();
    let payload: Vec<_> = (0..watch::ETH_MIN as u8).collect();
    for send in [true, false] {
        let until = m.now() + interval;
        assert_eq!(stores.advance(&mut m, until, |m, store| { check(m.pc(), store); false }), Ran::Tick);
        let mut out = Vec::new(); m.take_out(&mut out);
        let mut trace = Vec::new(); m.take_trace(&mut trace); m.mark();
        for byte in out {
            assert_eq!(byte.link, 0);
            if let Some(frame) = reader.push(byte.byte) {
                let frame = frame.expect("valid USB watch frame");
                match frame.event {
                    watch::Event::Hello { image, .. } => { assert_eq!(image, slot.image_id); epoch = Some(frame.tick); }
                    watch::Event::Fault { code, arg, .. } => reports.push((code, arg as u32)),
                    watch::Event::Eth { .. } => echoes.push(reader.eth_body().to_vec()),
                    _ => {}
                }
            }
        }
        if send {
            let mut wire = [0; watch::ETH_WIRE];
            let n = watch::encode_eth(0, epoch.expect("host receives Hello"), &payload, &mut wire).unwrap();
            for &byte in &wire[..n] { m.link_in(0, byte); }
        }
    }
    assert_eq!(reports, [(watch::FAULT_RADIO_ISLAND_MISMATCH, crc)], "one nonfatal report with the observed CRC");
    assert_eq!(ram_word(&m, slot.layout.state), 3, "setup and the timed handler both ran");
    assert_eq!(echoes, [payload], "the USB request receives its payload back after the failure");
    assert_eq!((ram_word(&m, r.generation), policy_word64(&m, r.next_start)), (0, 0),
        "106 leaves the island without a start marker or a scheduled retry");
    let mut actual = vec![0; poison.len()];
    assert!(m.read_data(SRAM_START, &mut actual) && actual == poison, "entire island window survived failed boot");
}

#[test]
fn erased_island_reports_absent_and_app_continues() {
    check_failed_boot(&[], radio::ISLAND_ABSENT, u32::MAX);
}

#[test]
fn changed_header_word_reports_mismatch_and_app_continues() {
    let island = island_fixture::small_island();
    for (offset, status) in [(24, radio::ISLAND_MISMATCH), (60, radio::ISLAND_MISMATCH), (0, radio::ISLAND_ABSENT)] {
        let mut flash = island.image.clone();
        flash[offset] ^= 1;
        let observed = u32::from_le_bytes(flash[60..64].try_into().unwrap());
        check_failed_boot(&flash, status, observed);
    }
}

#[test]
fn interrupted_island_write_never_reaches_ready() {
    // Boot flash snapshots in header-last order to exercise the start check independently of a flash writer.
    const SECTOR: usize = 4096;
    let header = fab_elec_mcu_island::Header::SIZE as usize;
    let old = island_fixture::linked_island(vec![0x35; SECTOR], 17);
    let new = island_fixture::linked_island(vec![0xca; SECTOR], 17);
    assert_eq!(old.header.abi_hash, new.header.abi_hash);
    assert_eq!(old.image.len(), new.image.len());
    assert!(new.image.len() > SECTOR && old.header.crc != new.header.crc);
    let app = app("fn setup() {}");
    let module = module::load(&app.module, &chip::profile()).unwrap();
    let build = |island: &Island| build_image(&module, &board(profile(island.binding())), &SlotOptions::default()).unwrap();
    let old_image = build(&old);
    let new_image = build(&new);
    let check = |image: &Image, flash: &[u8], status, boundary: &str| {
        let m = boot_past_island_start(image, flash);
        let r = image.slot.layout.radio.as_ref().unwrap();
        assert_eq!(ram_word(&m, r.island_status), status, "{boundary}: slot {:#x}", image.slot.image_id);
        let count = u32::from(status != radio::ISLAND_READY);
        assert_eq!([0, 4, 8].map(|off| ram_word(&m, r.reports + off)), [0, count, 0],
            "{boundary}: exact boot report count before the first drain");
        if count != 0 {
            let crc = u32::from_le_bytes(flash[60..64].try_into().unwrap());
            assert_eq!([12, 16, 20].map(|off| ram_word(&m, r.reports + off)),
                [u32::from(watch::FAULT_RADIO_ISLAND_MISMATCH), 0, crc], "{boundary}: boot report code, site and observed CRC");
        }
    };
    let incomplete = |flash: &[u8], status, boundary: &str| {
        for image in [&old_image, &new_image] { check(image, flash, status, boundary); }
    };
    let mut flash = old.image.clone();
    flash.resize(flash.len().next_multiple_of(SECTOR), 0xff);
    check(&old_image, &flash, radio::ISLAND_READY, "old island before update");
    for start in (0..new.image.len()).step_by(SECTOR) {
        // Erasing sector zero invalidates the old header before any new body byte is written.
        flash[start..start + SECTOR].fill(0xff);
        incomplete(&flash, radio::ISLAND_ABSENT, &format!("erased sector {}", start / SECTOR));
        let body = start.max(header)..(start + SECTOR).min(new.image.len());
        flash[body.clone()].copy_from_slice(&new.image[body]);
        incomplete(&flash, radio::ISLAND_ABSENT, &format!("programmed body sector {}", start / SECTOR));
    }
    assert_eq!(&flash[header..new.image.len()], &new.image[header..]);
    let mut torn = flash.clone();
    torn[..60].copy_from_slice(&new.image[..60]);
    assert_ne!(new.header.crc, u32::MAX);
    incomplete(&torn, radio::ISLAND_MISMATCH, "header interrupted before its CRC word");

    flash[..header].copy_from_slice(&new.image[..header]);
    assert_eq!(&flash[..new.image.len()], new.image.as_slice());
    check(&new_image, &flash, radio::ISLAND_READY, "committed new island");
    check(&old_image, &flash, radio::ISLAND_MISMATCH, "old slot before slot switch");
}

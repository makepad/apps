//! OSI layout and windowed ABI, without loading or running the radio pack.
use std::{collections::BTreeSet, sync::OnceLock};
use fab_elec_mcu::{compile_kernel, Compiled};
use fab_elec_mcu_emit32::xtensa::{assemble, reg::*, Asm};
use fab_elec_mcu_emit_s3::{build_kernel, chip, KernelCode, KernelOptions, KernelRegion, RADIO_KERNEL_LINES};
use fab_elec_mcu_emit_s3::radio::{self, adapter::{adapter, Entry, CRYPTO_FIELDS, OSI_FIELDS, OSI_MAGIC, OSI_VERSION,
    ESP_ERR_NVS_NOT_INITIALIZED, ESP_ERR_NVS_INVALID_HANDLE},
    kernel::{kernel_consts, KernelLayout, OBJECTS, TICKS_PER_US, RNG_DATA, RNG_SPACING_US,
        gates::{gates, EntryRegs, BLOCKING_EXPORTS}}};
use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};
use fab_elec_mcu_rt::{kernel::RegionKind, module::{load_kernel, Module}};
use fab_elec_mcu_s3::config::s3_config;
use fab_elec_mcu_xtensa::{insn::decode, sim::{Cpu, Stop, TrapMode}};

#[path = "support/heap_check.rs"] mod heap_check;

const BASE: u32 = 0x4000_1000;
const CALLER: u32 = 0x4000_0800;
const VECTORS: u32 = 0x4000_0000;
const RAM: u32 = 0x3ffb_0000;
const RAM_BYTES: usize = 0x3_0000;
const STATE: u32 = RAM + 0x1000;
const SHARED: u32 = RAM + 0x4000;
const HEAP: u32 = RAM + 0x8000;
const HEAP_BYTES: u32 = 2048;
const T0: u32 = RAM + 0x7000;
const TIMER: u32 = 0x6002_3000;
const REPORTS: u32 = RAM + 0x6000;
const STOP_PENDING: u32 = REPORTS + 12 + radio::REPORT_RING_ENTRIES * 12;

fn regions() -> Vec<KernelRegion> {
    use radio::kernel::{EFUSE_MAC_0, EFUSE_MAC_1, INTR_MATRIX, RTC_STORE1, SYSCON_WIFI_CLK_EN, SYSCON_WIFI_RST_EN, WIFI_SOURCES};
    vec![
        KernelRegion { base: RAM, len: RAM_BYTES as u32, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: SYSCON_WIFI_CLK_EN, len: SYSCON_WIFI_RST_EN - SYSCON_WIFI_CLK_EN + 4, writable: true, kind: RegionKind::Device },
        KernelRegion { base: INTR_MATRIX, len: WIFI_SOURCES * 4, writable: true, kind: RegionKind::Device },
        KernelRegion { base: EFUSE_MAC_0, len: EFUSE_MAC_1 - EFUSE_MAC_0 + 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: RTC_STORE1, len: 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: RNG_DATA, len: 4, writable: false, kind: RegionKind::Device },
    ]
}

fn compiled() -> &'static Compiled {
    static MODULE: OnceLock<Compiled> = OnceLock::new();
    MODULE.get_or_init(|| compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES,
        &kernel_consts(&KernelLayout { current: RAM, tasks: RAM + 0x100, task_count: RAM + 8,
            phy_cal_data: RAM + 0x9000, phy_dig_regs: RAM + 0x9800, phy_mutex: RAM + 0x300,
            isrs: RAM + 0x200, isr_count: RAM + 12, isr_capacity: radio::PACK_ISR_LINES, code_region: BASE >> 30,
            max_tasks: 6, heap_base: HEAP, heap_size: HEAP_BYTES, timer_reserve: 0 }))
        .unwrap_or_else(|errors| panic!("OSI kernel: {errors:?}")))
}

// Fields the kept station code reads on the S3 (pack census, IDF v6.1.1 libraries).
const READ_FIELDS: [&str; 97] = [
    "_env_is_chip", "_set_intr", "_clear_intr", "_set_isr", "_ints_on", "_ints_off",
    "_spin_lock_create", "_spin_lock_delete", "_wifi_int_disable", "_wifi_int_restore",
    "_task_yield_from_isr", "_semphr_create", "_semphr_delete", "_semphr_take", "_semphr_give",
    "_wifi_thread_semphr_get", "_mutex_create", "_recursive_mutex_create", "_mutex_delete",
    "_mutex_lock", "_mutex_unlock", "_queue_send", "_queue_send_from_isr", "_queue_recv",
    "_queue_msg_waiting", "_task_create_pinned_to_core", "_task_delete", "_task_delay",
    "_task_ms_to_tick", "_task_get_current_task", "_task_get_max_priority", "_malloc", "_free",
    "_event_post", "_rand", "_wifi_pm_sleep_lock_acquire", "_wifi_pm_sleep_lock_release",
    "_phy_disable", "_phy_enable", "_phy_update_country_info", "_read_mac", "_timer_arm",
    "_timer_disarm", "_timer_done", "_timer_setfn", "_timer_arm_us", "_wifi_reset_mac",
    "_wifi_clock_enable", "_wifi_clock_disable", "_wifi_rtc_enable_iso", "_wifi_rtc_disable_iso",
    "_esp_timer_get_time", "_nvs_set_i8", "_nvs_get_i8", "_nvs_set_u8", "_nvs_get_u8",
    "_nvs_set_u16", "_nvs_get_u16", "_nvs_open", "_nvs_close", "_nvs_commit", "_nvs_set_blob",
    "_nvs_get_blob", "_get_random", "_get_time", "_random", "_slowclk_cal_get", "_log_write",
    "_log_writev", "_log_timestamp", "_malloc_internal", "_calloc_internal", "_zalloc_internal",
    "_wifi_malloc", "_wifi_zalloc", "_wifi_create_queue", "_wifi_delete_queue", "_coex_enable",
    "_coex_disable", "_coex_status_get", "_coex_wifi_request", "_coex_wifi_release",
    "_coex_wifi_channel_set", "_coex_event_duration_get", "_coex_pti_get",
    "_coex_schm_status_bit_clear", "_coex_schm_status_bit_set", "_coex_schm_interval_set",
    "_coex_schm_interval_get", "_coex_schm_curr_period_get", "_coex_schm_curr_phase_get",
    "_coex_schm_process_restart", "_coex_schm_register_cb", "_coex_register_start_cb",
    "_coex_schm_flexible_period_set", "_coex_schm_flexible_period_get", "_coex_schm_get_phase_by_idx",
];

#[test]
fn s3_table_and_exports() {
    assert_eq!(OSI_FIELDS.len(), 120);
    let names: BTreeSet<_> = OSI_FIELDS.iter().map(|&(name, _)| name).collect();
    assert_eq!(names.len(), 120, "every field has a unique name");
    assert_eq!(OSI_FIELDS[0], ("_version", Entry::Data(OSI_VERSION)));
    assert_eq!(OSI_FIELDS[119], ("_magic", Entry::Data(OSI_MAGIC)));
    assert_eq!(READ_FIELDS.iter().collect::<BTreeSet<_>>().len(), 97);
    for name in READ_FIELDS {
        let (_, entry) = OSI_FIELDS.iter().find(|&&(field, _)| field == name).expect(name);
        assert_ne!(*entry, Entry::Fault, "read field {name} cannot be Fault");
    }
    let module = load_kernel(&compiled().module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    for (name, entry) in OSI_FIELDS {
        for &target in entry.kernel_exports() {
            assert!(module.entries().iter().any(|e| module.string(e.name) == target), "{name}: missing {target}");
            if !matches!(entry, Entry::Gate(_)) {
                assert!(!BLOCKING_EXPORTS.contains(&target), "{name}: non-blocking thunk required");
            }
        }
    }
    let row = |name| OSI_FIELDS.iter().find(|&&(field, _)| field == name).expect(name).1;
    for name in ["_clear_intr", "_wifi_rtc_enable_iso", "_wifi_rtc_disable_iso",
        "_wifi_clock_enable", "_wifi_clock_disable", "_nvs_close", "_log_write", "_log_writev",
        "_coex_enable", "_coex_disable", "_coex_status_get", "_coex_wifi_request",
        "_coex_wifi_release", "_coex_wifi_channel_set", "_coex_event_duration_get", "_coex_pti_get",
        "_coex_schm_status_bit_clear", "_coex_schm_status_bit_set", "_coex_schm_interval_set",
        "_coex_schm_interval_get", "_coex_schm_curr_period_get", "_coex_schm_curr_phase_get",
        "_coex_schm_process_restart", "_coex_schm_register_cb", "_coex_register_start_cb",
        "_coex_schm_flexible_period_set", "_coex_schm_get_phase_by_idx"] {
        assert_eq!(row(name), Entry::Const(0), "{name}: no-op/zero/NULL without optional services");
    }
    assert_eq!(row("_coex_schm_flexible_period_get"), Entry::Const(1), "IDF no-coex period");
    assert_eq!(ESP_ERR_NVS_NOT_INITIALIZED, 0x1101);
    assert_eq!(ESP_ERR_NVS_INVALID_HANDLE, 0x1107);
    assert_eq!(row("_nvs_open"), Entry::Const(0x1101));
    for name in ["_nvs_set_i8", "_nvs_get_i8", "_nvs_set_u8", "_nvs_get_u8",
        "_nvs_set_u16", "_nvs_get_u16", "_nvs_commit", "_nvs_set_blob", "_nvs_get_blob"] {
        assert_eq!(row(name), Entry::Const(0x1107), "{name}: no handle was issued");
    }
    for (name, target) in [("_log_timestamp", "osi_log_timestamp"), ("_ints_on", "osi_ints_on"),
        ("_ints_off", "osi_ints_off"), ("_wifi_reset_mac", "osi_wifi_reset_mac"),
        ("_read_mac", "osi_read_mac"), ("_slowclk_cal_get", "osi_slowclk_cal_get"),
        ("_rand", "osi_random"), ("_random", "osi_random")] {
        assert_eq!(row(name), Entry::Gate(target), "{name}: device export");
    }
    assert_eq!(row("_set_intr"), Entry::Gate("osi_set_intr"));
    assert_eq!(row("_set_isr"), Entry::Gate("osi_set_isr"));
    assert_eq!(row("_wifi_int_disable"), Entry::Gate("osi_wifi_int_disable"));
    assert_eq!(row("_wifi_int_restore"), Entry::Gate("osi_wifi_int_restore"));
    assert_eq!(row("_get_random"), Entry::Chunked("osi_random"));
    for (internal, wifi, entry) in [("_calloc_internal", "_wifi_calloc", Entry::Calloc),
        ("_zalloc_internal", "_wifi_zalloc", Entry::Zalloc), ("_realloc_internal", "_wifi_realloc", Entry::Realloc)] {
        assert_eq!(row(internal), entry, "{internal}");
        assert_eq!(row(wifi), entry, "{wifi}");
    }
    assert_eq!(row("_phy_enable"), Entry::Glue("phy_enable_glue"));
    assert_eq!(row("_phy_disable"), Entry::Glue("phy_disable_glue"));
    assert_eq!(OSI_FIELDS.iter().filter(|(_, entry)| matches!(entry, Entry::Pending(_))).count(), 0);
    assert_eq!(row("_event_post"), Entry::Glue("event_post_glue"));
    for name in ["osi_semphr_take", "osi_mutex_lock", "osi_queue_send_to_back", "osi_queue_send_to_front",
        "osi_queue_recv", "osi_task_delay", "osi_task_delete"] {
        assert!(BLOCKING_EXPORTS.contains(&name), "{name}: blocking thunk required");
    }
    let phy = radio::phy_glue::phy_glue();
    for &target in phy.gates {
        assert!(module.entries().iter().any(|e| module.string(e.name) == target), "PHY glue: missing {target}");
    }
    let glue = radio::island_glue();
    let glue_entry = |name: &str| {
        let index = glue.iter().position(|object| object.name == name).expect("OSI glue exists");
        0x4020_0000 + index as u32 * 0x1000 + glue[index].entry
    };
    let built = adapter(BASE, |target| 0x4010_0000 + target.len() as u32 * 4, glue_entry);
    assert_eq!(built.crypto.into_iter().collect::<BTreeSet<_>>().len(), 11);
    for ((field, row), address) in CRYPTO_FIELDS.iter().zip(built.crypto) {
        assert_eq!(*row, Entry::Fault);
        assert!(built.symbols.iter().any(|(name, at, len)| name == &format!("wifi_crypto_fault_{field}")
            && *at == address && *len == 9));
    }
    for (index, (_, entry)) in OSI_FIELDS.iter().enumerate() {
        match entry {
            Entry::Data(value) => assert_eq!(built.table[index], *value),
            Entry::Gate(target) | Entry::Gate64(target) => assert_eq!(built.table[index], 0x4010_0000 + target.len() as u32 * 4),
            Entry::Glue(target) => assert_eq!(built.table[index], glue_entry(target), "glue word includes its entry offset"),
            _ => {}
        }
    }
}

fn map(memory: &mut Memory, base: u32, bytes: &[u8], writable: bool) -> usize {
    // Fetches may read a whole word at the last two-byte instruction.
    let store = memory.store(bytes.len() + 4);
    memory.bytes_mut(store)[..bytes.len()].copy_from_slice(bytes);
    memory.map(base, store, 0, bytes.len() + 4, Perm { read: true, write: writable, exec: !writable });
    store
}
fn put(memory: &mut Memory, ram: usize, address: u32, value: u32) {
    let offset = (address - RAM) as usize;
    memory.bytes_mut(ram)[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn kernel_ram(memory: &mut Memory, module: &Module<'_>, objects: &KernelCode) -> usize {
    let ram = map(memory, RAM, &vec![0; RAM_BYTES], true);
    for (base, words) in [(STATE, module.state_init()), (SHARED, module.shared())] {
        for (i, &word) in words.iter().enumerate() { put(memory, ram, base + i as u32 * 4, word); }
    }
    put(memory, ram, objects.task_exit_link.unwrap(), radio::windowed_exit_link(BASE));
    put(memory, ram, objects.phy_init_data.unwrap(), 0); // No PHY table in this fixture.
    assert!(memory.bytes(ram)[(HEAP - RAM) as usize..(HEAP - RAM + HEAP_BYTES) as usize].iter().all(|&b| b == 0),
        "adapter kernel_ram must supply the zeroed heap required by objects.splash");
    ram
}
fn get(memory: &Memory, ram: usize, address: u32) -> u32 {
    let offset = (address - RAM) as usize;
    u32::from_le_bytes(memory.bytes(ram)[offset..offset + 4].try_into().unwrap())
}
fn caller(memory: &mut Memory, target: u32) -> (Cpu, u32) {
    let (mut entry, mut done) = (0, 0);
    let bytes = assemble(&mut Asm::new(0, A8), |a| {
        a.literal_table();
        entry = CALLER + a.base.offset();
        a.entry(A1, 32);
        a.callx8(A8);
        done = CALLER + a.base.offset();
        a.op24(0x004000); // BREAK at the windowed caller's return site.
    });
    map(memory, CALLER, &bytes, false);
    let mut cpu = Cpu::new(s3_config(), entry);
    cpu.vecbase = VECTORS;
    cpu.ps = (1 << 18) | (1 << 5);
    cpu.window_base = 15;
    cpu.window_start = 1 << 15;
    cpu.ar[1] = RAM + RAM_BYTES as u32 - 16;
    cpu.ar[8] = target;
    cpu.cpenable = 1;
    (cpu, done)
}

struct HeapRows {
    memory: Memory,
    ram: usize,
    caller: Cpu,
    done: u32,
    rows: radio::adapter::Adapter,
    call_bound: u64,
}

impl HeapRows {
    fn new() -> Self {
        let module = load_kernel(&compiled().module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
        let objects = build_kernel(&module, &KernelOptions { base: 0x4010_0000, state: STATE, t0: T0, reports: Some(REPORTS), stop_pending: Some(STOP_PENDING),
            regions: regions() }).unwrap();
        let gate_base = (objects.base + objects.bytes.len() as u32 + 7) & !3;
        let routines = gates(gate_base, &objects, EntryRegs { state: STATE, shared: SHARED });
        let mut imports = Vec::new();
        let mut gate_bound = 0;
        for (export, gate) in routines.gates.iter().enumerate() {
            if !matches!(gate.name.as_str(), "malloc" | "free" | "calloc_reserve" | "realloc_reserve"
                | "heap_request_bytes" | "heap_free_bytes") { continue; }
            imports.push(radio::Gate { name: &gate.name, call: gate.address, blocks: gate.blocks,
                exit_level: None, export: export.try_into().unwrap() });
            gate_bound += gate.max_insns;
        }
        let thunk_base = (gate_base + routines.bytes.len() as u32 + 7) & !3;
        let thunks = radio::exports(thunk_base, &[], &imports, None, None);
        let rows = adapter(BASE, |name| imports.iter().position(|g| g.name == name)
            .map_or(0, |i| thunks.thunks[i]), |_| 0);
        let mut memory = Memory::new();
        let ram = kernel_ram(&mut memory, &module, &objects);
        map(&mut memory, objects.base, &objects.bytes, false);
        map(&mut memory, gate_base, &routines.bytes, false);
        map(&mut memory, thunk_base, &thunks.bytes, false);
        map(&mut memory, BASE, &rows.bytes, false);
        let (caller, done) = caller(&mut memory, 0);
        // Each row calls each of its thunks at most once. Allow its straight-line
        // code and up to eight loop instructions per payload byte.
        let call_bound = gate_bound + (thunks.bytes.len() + rows.bytes.len()) as u64 + 8 * u64::from(HEAP_BYTES) + 4;
        Self { memory, ram, caller, done, rows, call_bound }
    }

    fn call(&mut self, field: &str, args: &[u32]) -> u32 {
        let index = OSI_FIELDS.iter().position(|&(name, _)| name == field).unwrap();
        let mut cpu = self.caller.clone();
        cpu.ar[8] = self.rows.table[index];
        cpu.ar[10..10 + args.len()].copy_from_slice(args);
        let adapter_code = BASE..BASE + self.rows.bytes.len() as u32;
        for _ in 0..self.call_bound {
            if adapter_code.contains(&cpu.pc) {
                assert_eq!(cpu.ps & 15, self.caller.ps & 15, "{field}: payload work at caller INTLEVEL");
            }
            match cpu.step(&mut self.memory) {
                Ok(()) => {}
                Err(Stop::Break { pc }) => {
                    assert_eq!(pc, self.done, "{field}: returns to the caller");
                    assert_eq!(cpu.ps & 15, self.caller.ps & 15, "{field}: restores caller INTLEVEL");
                    return cpu.ar[10];
                }
                Err(stop) => panic!("{field}{args:?}: {stop:?}"),
            }
        }
        panic!("{field}{args:?}: exceeded the gate and payload loop bound")
    }

    fn payload(p: u32, size: usize) -> std::ops::Range<usize> {
        let offset = p.checked_sub(HEAP).expect("allocation starts inside the heap") as usize;
        assert!(offset + size <= HEAP_BYTES as usize, "allocation ends inside the heap");
        let start = (HEAP - RAM) as usize + offset;
        start..start + size
    }
    fn bytes_mut(&mut self, p: u32, size: usize) -> &mut [u8] {
        &mut self.memory.bytes_mut(self.ram)[Self::payload(p, size)]
    }

    fn check_heap(&self, seed: u32, step: usize) {
        let status = heap_check::heap_check(|at| get(&self.memory, self.ram, at),
            |at| self.memory.bytes(self.ram)[(at - RAM) as usize], HEAP, HEAP_BYTES);
        assert_eq!(status, 0, "seed {seed:#x}, operation {step}: heap invariant");
    }
}

#[derive(Clone, Copy)]
struct Allocation { address: u32, size: usize, tag: u8 }
fn pattern(tag: u8, offset: usize) -> u8 { tag.wrapping_add((offset as u8).wrapping_mul(37)) ^ (offset >> 8) as u8 }
fn write_pattern(heap: &mut HeapRows, allocation: Allocation) {
    for (i, byte) in heap.bytes_mut(allocation.address, allocation.size).iter_mut().enumerate() {
        *byte = pattern(allocation.tag, i);
    }
}
fn assert_pattern(heap: &HeapRows, allocation: Allocation, bytes: usize, seed: u32, step: usize) {
    let range = HeapRows::payload(allocation.address, bytes);
    for (i, &byte) in heap.memory.bytes(heap.ram)[range].iter().enumerate() {
        assert_eq!(byte, pattern(allocation.tag, i), "seed {seed:#x}, operation {step}: live {:#x}, byte {i}", allocation.address);
    }
}
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
    fn size(&mut self) -> usize {
        match self.next() % 8 {
            0 => 0, 1 => HEAP_BYTES as usize, 2 => 257 + (self.next() % 768) as usize,
            _ => 1 + (self.next() % 192) as usize,
        }
    }
}
fn heap_fuzz(heap: &mut HeapRows, seed: u32) {
    let initial = heap.call("_get_free_heap_size", &[]);
    let failures = get(&heap.memory, heap.ram, HEAP + 4); // H_FAILURES
    let mut rng = Rng(seed);
    let mut live = [None::<Allocation>; 12];
    for step in 0..2000 {
        let index = rng.next() as usize % live.len();
        let operation = rng.next() % 4;
        if operation == 2 {
            let old = live[index];
            let size = rng.size();
            let p = heap.call("_wifi_realloc", &[old.map_or(0, |a| a.address), size as u32]);
            heap.check_heap(seed, step);
            if old.is_some() && size == 0 {
                assert_eq!(p, 0, "seed {seed:#x}, operation {step}: realloc frees at zero");
                live[index] = None;
            } else if p != 0 {
                let tag = old.map_or(rng.next() as u8, |a| a.tag);
                let next = Allocation { address: p, size, tag };
                if let Some(old) = old { assert_pattern(heap, next, old.size.min(size), seed, step); }
                write_pattern(heap, next);
                live[index] = Some(next);
            } // failed realloc retains the old block and its pattern
        } else {
            if let Some(old) = live[index].take() {
                heap.call("_free", &[old.address]);
                heap.check_heap(seed, step);
            }
            if operation != 1 {
                let (p, size, zeroed) = if operation == 0 {
                    let size = rng.size();
                    (heap.call("_malloc", &[size as u32]), size, false)
                } else if rng.next() & 15 == 0 {
                    let p = heap.call("_calloc_internal", &[u32::MAX, 2]);
                    assert_eq!(p, 0, "overflowing calloc returns null");
                    (p, 0, true)
                } else {
                    let n = rng.next() % 9;
                    let size = rng.next() % 160;
                    (heap.call("_calloc_internal", &[n, size]), (n * size) as usize, true)
                };
                heap.check_heap(seed, step);
                if p != 0 {
                    assert_eq!(p & 7, 0, "seed {seed:#x}, operation {step}: eight-byte alignment");
                    if zeroed {
                        assert!(heap.bytes_mut(p, size).iter().all(|&b| b == 0),
                            "seed {seed:#x}, operation {step}: calloc zeroes every byte");
                    }
                    let next = Allocation { address: p, size, tag: rng.next() as u8 };
                    write_pattern(heap, next);
                    live[index] = Some(next);
                }
            }
        }
        for allocation in live.iter().flatten() { assert_pattern(heap, *allocation, allocation.size, seed, step); }
    }
    for allocation in live.into_iter().flatten() {
        heap.call("_free", &[allocation.address]);
        heap.check_heap(seed, 2000);
    }
    assert!(get(&heap.memory, heap.ram, HEAP + 4) > failures, "seed {seed:#x}: exhaustion exercised");
    assert_eq!(heap.call("_get_free_heap_size", &[]), initial, "seed {seed:#x}: all bytes reclaimed");
    let whole = heap.call("_malloc", &[initial]);
    assert_ne!(whole, 0, "seed {seed:#x}: all freed memory is one block again");
    heap.check_heap(seed, 2001);
    assert_eq!(heap.call("_get_free_heap_size", &[]), 0);
    heap.call("_free", &[whole]);
    heap.check_heap(seed, 2002);
    assert_eq!(heap.call("_get_free_heap_size", &[]), initial);
}

#[test]
fn heap_rows_preserve_payloads() {
    let mut heap = HeapRows::new();
    heap_fuzz(&mut heap, 0x1234_5678);
    heap.caller.ps |= 3;
    heap_fuzz(&mut heap, 0x9e37_79b9);
    let initial = heap.call("_get_free_heap_size", &[]);
    let a = heap.call("_realloc_internal", &[0, 303]);
    assert_ne!(a, 0, "realloc(null, size)");
    write_pattern(&mut heap, Allocation { address: a, size: 303, tag: 0x71 });
    let barrier = heap.call("_malloc", &[16]);
    assert_ne!(barrier, 0);
    write_pattern(&mut heap, Allocation { address: barrier, size: 16, tag: 0xc3 });
    let moved = heap.call("_realloc_internal", &[a, 700]);
    assert_ne!(moved, 0);
    assert_ne!(moved, a, "live neighbour forces a move to a new block");
    assert_pattern(&heap, Allocation { address: moved, size: 700, tag: 0x71 }, 303, 0, 0);
    assert_pattern(&heap, Allocation { address: barrier, size: 16, tag: 0xc3 }, 16, 0, 0);
    write_pattern(&mut heap, Allocation { address: moved, size: 700, tag: 0x71 });
    assert_eq!(heap.call("_realloc_internal", &[moved, u32::MAX]), 0, "failed realloc retains storage");
    assert_pattern(&heap, Allocation { address: moved, size: 700, tag: 0x71 }, 700, 0, 0);
    assert_eq!(heap.call("_realloc_internal", &[moved, 257]), moved, "shrink splits and coalesces the tail");
    assert_pattern(&heap, Allocation { address: moved, size: 257, tag: 0x71 }, 257, 0, 0);
    heap.check_heap(0, 0);
    assert_eq!(heap.call("_realloc_internal", &[moved, 650]), moved, "grow into the adjacent free tail");
    assert_pattern(&heap, Allocation { address: moved, size: 650, tag: 0x71 }, 257, 0, 0);
    heap.check_heap(0, 0);
    assert_eq!(heap.call("_realloc_internal", &[moved, 0]), 0, "realloc(p, 0) frees");
    heap.call("_free", &[barrier]);
    for (name, padding) in [("_zalloc_internal", 0), ("_wifi_calloc", 0), ("_wifi_zalloc", 1), ("_calloc_internal", 3)] {
        let dirty = heap.call("_malloc", &[initial]);
        heap.bytes_mut(dirty, initial as usize).fill(0xa5);
        heap.call("_free", &[dirty]);
        let size = initial - padding;
        let args = if name.contains("calloc") { vec![1, size] } else { vec![size] };
        let p = heap.call(name, &args);
        assert_eq!(p, dirty, "{name}: reuse the only block");
        let (payload, spare) = heap.bytes_mut(p, initial as usize).split_at(size as usize);
        assert!(payload.iter().all(|&b| b == 0), "{name}: every requested byte zeroed");
        assert!(spare.iter().all(|&b| b == 0xa5), "{name}: bytes past the requested tail stay dirty");
        heap.check_heap(0, 0);
        heap.call("_free", &[p]);
    }
    assert_eq!(heap.call("_get_free_heap_size", &[]), initial);
    heap.check_heap(0, 0);
}

#[test]
fn all_stubs_through_callx8() {
    let built = adapter(BASE, |_| 0, |_| 0);
    for (index, (field, entry)) in OSI_FIELDS.iter().enumerate() {
        let kind = match entry {
            Entry::Const(_) => "const", Entry::Fault => "fault", Entry::Pending(_) => "pending",
            _ => continue,
        };
        let symbol = built.symbols.iter().find(|s| s.0 == format!("osi_{kind}_{field}")).unwrap();
        assert_eq!(built.table[index], symbol.1, "{field}: table points at its symbol");
        let mut memory = Memory::new();
        map(&mut memory, RAM, &vec![0; RAM_BYTES], true);
        map(&mut memory, BASE, &built.bytes, false);
        let (mut cpu, done) = caller(&mut memory, built.table[index]);
        let mut stopped = false;
        for _ in 0..32 {
            match cpu.step(&mut memory) {
                Ok(()) => {}
                Err(Stop::Break { pc }) => {
                    assert_eq!(pc, done, "{field}: caller return site");
                    let Entry::Const(value) = entry else { panic!("{field}: fault stub returned") };
                    assert_eq!(cpu.ar[10], *value as u32, "{field}: CALL8 return value");
                    assert_eq!(cpu.window_base, 15, "{field}: caller window restored");
                    stopped = true; break;
                }
                Err(Stop::Exception { cause: 0, pc, .. }) => {
                    assert!(matches!(entry, Entry::Fault | Entry::Pending(_)), "{field}: constant faulted");
                    assert_eq!(cpu.ar[2], index as u32, "{field}: diagnostic index");
                    assert!((symbol.1..symbol.1 + symbol.2).contains(&pc), "{field}: symbol covers ILL");
                    stopped = true; break;
                }
                Err(stop) => panic!("{field}: unexpected {stop:?}"),
            }
        }
        assert!(stopped, "{field}: stub did not return or fault");
    }
}

struct ClockMemory { memory: Memory, updates: usize }
impl Bus for ClockMemory {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        self.memory.read(addr, width, kind, now)
    }
    fn write(&mut self, addr: u32, width: Width, mut value: u32, now: u64) -> Result<(), ()> {
        if addr == TIMER + 4 {
            assert_eq!((width, value), (Width::W, 1 << 30));
            value |= 1 << 29;
            self.updates += 1;
        }
        self.memory.write(addr, width, value, now)
    }
}

#[test]
fn gate64_through_real_gate_and_windowed_thunk() {
    let module = load_kernel(&compiled().module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let objects = build_kernel(&module, &KernelOptions { base: 0x4010_0000, state: STATE, t0: T0, reports: Some(REPORTS), stop_pending: Some(STOP_PENDING),
        regions: regions() }).unwrap();
    let gate_base = (objects.base + objects.bytes.len() as u32 + 7) & !3;
    let routines = gates(gate_base, &objects, EntryRegs { state: STATE, shared: SHARED });
    let gate = routines.gates.iter().find(|g| g.name == "osi_esp_timer_get_time").unwrap();
    assert!(!gate.blocks);
    let thunk_base = (gate_base + routines.bytes.len() as u32 + 7) & !3;
    // Independent OSI thunk set; no island import gate names participate.
    let thunks = radio::exports(thunk_base, &[], &[radio::Gate {
        name: &gate.name, call: gate.address, blocks: gate.blocks,
        exit_level: None, export: objects.entries.iter().position(|e| e.name == gate.name).unwrap().try_into().unwrap(),
    }], None, None);
    let built = adapter(BASE, |target| if target == gate.name.as_str() { thunks.thunks[0] } else { 0 }, |_| 0);
    for (snapshot, boot) in [(0x000f_ffff_0000_0010u64, 0x123_0000_0020u64), (16 * 1_234_567 + 15, 0)] {
        let mut memory = Memory::new();
        let ram = kernel_ram(&mut memory, &module, &objects);
        put(&mut memory, ram, T0, boot as u32);
        put(&mut memory, ram, T0 + 4, (boot >> 32) as u32);
        map(&mut memory, objects.base, &objects.bytes, false);
        map(&mut memory, gate_base, &routines.bytes, false);
        map(&mut memory, thunk_base, &thunks.bytes, false);
        let mut timer = vec![0; 0x100];
        timer[0x40..0x44].copy_from_slice(&((snapshot >> 32) as u32).to_le_bytes());
        timer[0x44..0x48].copy_from_slice(&(snapshot as u32).to_le_bytes());
        map(&mut memory, TIMER, &timer, true);
        let (mut cpu, done) = caller(&mut memory, built.table[66]);
        let mut bus = ClockMemory { memory, updates: 0 };
        let mut stopped = false;
        for _ in 0..gate.max_insns + 64 {
            match cpu.step(&mut bus) {
                Ok(()) => {}
                Err(Stop::Break { pc }) => { assert_eq!(pc, done); stopped = true; break; }
                Err(stop) => panic!("Gate64: {stop:?}"),
            }
        }
        assert!(stopped, "Gate64 returned within its bound plus caller/thunk");
        let expected = (snapshot - boot) / u64::from(TICKS_PER_US);
        assert_eq!(u64::from(cpu.ar[10]) | (u64::from(cpu.ar[11]) << 32), expected,
            "a3 survives the non-blocking thunk and becomes caller a11");
        assert_eq!(get(&bus.memory, ram, objects.osi_time_high.unwrap()), (expected >> 32) as u32);
        assert_eq!(bus.updates, 1, "one coherent SYSTIMER snapshot");
        assert_eq!(cpu.ps & 15, 0, "thunk restores interrupt level");
    }
}

#[test]
fn exit_level_thunks_pass_hidden_arguments_and_preserve_other_ps_fields() {
    use fab_elec_mcu_emit_s3::{EntryKind, radio::kernel::gates::EXIT_LEVEL_EXPORTS};
    use fab_elec_mcu_xtensa::insn::Op;
    let module = load_kernel(&compiled().module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let objects = build_kernel(&module, &KernelOptions { base: 0x4010_0000, state: STATE, t0: T0, reports: Some(REPORTS), stop_pending: Some(STOP_PENDING),
        regions: regions() }).unwrap();
    let gate_base = (objects.base + objects.bytes.len() as u32 + 7) & !3;
    let routines = gates(gate_base, &objects, EntryRegs { state: STATE, shared: SHARED });
    let imports: Vec<_> = EXIT_LEVEL_EXPORTS.iter().zip([1u8, 2]).map(|(&name, n)| {
        let export = objects.entries.iter().position(|e| e.name == name).unwrap();
        let EntryKind::Export { params, .. } = &objects.entries[export].kind else { unreachable!() };
        assert_eq!(params.len(), usize::from(n) + 3, "pack arguments plus entry PS, RA and CCOUNT");
        let gate = routines.gates.iter().find(|g| g.name == name).unwrap();
        radio::Gate { name, call: gate.address, blocks: gate.blocks, exit_level: Some(n), export: export.try_into().unwrap() }
    }).collect();
    let thunk_base = (gate_base + routines.bytes.len() as u32 + 7) & !3;
    let thunks = radio::exports(thunk_base, &[], &imports, None, None);
    let ordinary: Vec<_> = imports.iter().map(|g| radio::Gate { exit_level: None,
        name: g.name, call: g.call, blocks: g.blocks, export: g.export }).collect();
    assert_eq!(thunks.abi_hash, radio::exports(thunk_base, &[], &ordinary, None, None).abi_hash,
        "exit-level metadata does not enter the island ABI hash");
    let outer_slot = STATE + 4 * module.vars().iter().find(|v| module.string(v.name) == "crit_outer").unwrap().offset;
    for outer in [0u32, 3, 5] {
        let raised = outer.max(3);
        // Both disable depths and both restore depths, including a restore
        // at depth zero. Each case starts from explicit pre-execution state.
        for (which, level, depth, token, next_depth, exit, result) in [
            (0, outer, 0, 0, 1, raised, outer), (0, raised, 1, 0, 2, raised, raised),
            (1, raised, 2, raised, 1, raised, 0), (1, raised, 1, outer, 0, outer, 0),
            (1, outer, 0, 0, 0, outer, 0),
        ] {
            let import = &imports[which];
            let EntryKind::Export { params, .. } = &objects.entries[usize::from(import.export)].kind else { unreachable!() };
            let mut memory = Memory::new();
            let ram = kernel_ram(&mut memory, &module, &objects);
            put(&mut memory, ram, objects.crit_depth.unwrap(), depth);
            put(&mut memory, ram, outer_slot, outer);
            map(&mut memory, objects.base, &objects.bytes, false);
            map(&mut memory, gate_base, &routines.bytes, false);
            map(&mut memory, thunk_base, &thunks.bytes, false);
            let (mut cpu, done) = caller(&mut memory, thunks.thunks[which]);
            cpu.ps |= level | (9 << 8); // exercise the saved OWB field as well as WOE/UM/CALLINC
            cpu.ar[10] = 0x1234; // ignored mux must still arrive intact
            cpu.ar[11] = token;
            cpu.ccount = 0xffff_fff0; // the real RSR observes the model clock, including wrap
            let (mut entry_ps, mut restored, mut returned) = (None, false, false);
            let mut entry_cc = None;
            let bound = routines.gates.iter().find(|g| g.name == import.name).unwrap().max_insns + 128;
            for _ in 0..bound {
                let pc = cpu.pc;
                let mut is_restore = false;
                if (thunk_base..thunk_base + thunks.bytes.len() as u32).contains(&pc) {
                    let at = (pc - thunk_base) as usize;
                    let mut bytes = [0; 4];
                    let len = 3.min(thunks.bytes.len() - at);
                    bytes[..len].copy_from_slice(&thunks.bytes[at..at + len]);
                    let (insn, _) = decode(u32::from_le_bytes(bytes)).unwrap();
                    if insn.op == Op::Rsil { entry_ps = Some(cpu.ps); }
                    if insn.op == Op::Rsr && insn.imm == 234 {
                        assert_eq!(pc, thunks.thunks[which] + 9, "CCOUNT is the instruction immediately after RSIL");
                        entry_cc = Some(cpu.ccount);
                    }
                    is_restore = insn.op == Op::Wsr && insn.imm == 230;
                }
                match cpu.step(&mut memory) {
                    Ok(()) => {}
                    Err(Stop::Break { pc }) => { assert_eq!(pc, done); returned = true; break; }
                    Err(stop) => panic!("{}: {stop:?}", import.name),
                }
                if is_restore {
                    assert_eq!(cpu.ps, (entry_ps.unwrap() & !15) | exit, "only PS.INTLEVEL changes at WSR");
                    restored = true;
                }
            }
            assert!(restored && returned);
            assert_eq!(cpu.ar[10], result, "CALL8 returns the token in the pack's a10");
            assert_eq!(cpu.ps & 15, exit);
            let n = usize::from(import.exit_level.unwrap());
            assert_eq!(get(&memory, ram, params[0]), 0x1234);
            if n == 2 { assert_eq!(get(&memory, ram, params[1]), token); }
            assert_eq!(get(&memory, ram, params[n]), entry_ps.unwrap());
            assert_eq!(get(&memory, ram, params[n + 1]), done, "CALL8 tag replaced by the thunk's region");
            assert_eq!(get(&memory, ram, params[n + 2]), entry_cc.unwrap(), "third hidden argument is the CCOUNT stamp");
            assert_eq!(get(&memory, ram, objects.crit_depth.unwrap()), next_depth);
            assert_eq!(get(&memory, ram, objects.osi_crit_level.unwrap()), exit);
        }
    }
}

#[test]
fn blocking_gate_faults_only_when_it_would_wait_in_a_critical_section() {
    use fab_elec_mcu_emit_s3::radio::*;
    use fab_elec_mcu_rt::{kernel::KernelFault, sched::Fault, watch::FAULT_KERNEL_BLOCK_IN_CRITICAL};
    const TASK: u32 = RAM + 0x800;
    const SCHED: u32 = 0x4004_0000;
    let module = load_kernel(&compiled().module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let objects = build_kernel(&module, &KernelOptions { base: 0x4010_0000, state: STATE, t0: T0, reports: Some(REPORTS), stop_pending: Some(STOP_PENDING),
        regions: regions() }).unwrap();
    let gate_base = (objects.base + objects.bytes.len() as u32 + 7) & !3;
    let routines = gates(gate_base, &objects, EntryRegs { state: STATE, shared: SHARED });
    let gate = routines.gates.iter().find(|g| g.name == "osi_task_delay").unwrap();
    let export: u16 = objects.entries.iter().position(|e| e.name == gate.name).unwrap().try_into().unwrap();
    let sched = sched_code(SCHED, &SchedLayout { current: RAM, cursor: RAM + 4, task_count: RAM + 8,
        tasks: RAM + 0x100, isrs: RAM + 0x200, isr_count: RAM + 12, stray: RAM + 16,
        dispatcher: TASK, wake_lines: 0, vector_lines: 0, shim: BASE, marks: None });
    let (_, fault, fault_size) = objects.symbols.iter().find(|s| s.0 == "kernel_fault").unwrap();
    let thunk_base = (gate_base + routines.bytes.len() as u32 + 7) & !3;
    let thunks = exports(thunk_base, &[], &[Gate { name: &gate.name, call: gate.address, blocks: gate.blocks,
        exit_level: None, export }], None, Some(Blocking { current: RAM, block: sched.block,
            crit_depth: objects.crit_depth, kernel_fault: *fault }));
    for (depth, ticks) in [(1, 1), (0, 1), (1, 0)] {
        let mut memory = Memory::new();
        let ram = kernel_ram(&mut memory, &module, &objects);
        put(&mut memory, ram, RAM, TASK);
        put(&mut memory, ram, objects.crit_depth.unwrap(), depth);
        map(&mut memory, objects.base, &objects.bytes, false);
        map(&mut memory, gate_base, &routines.bytes, false);
        map(&mut memory, thunk_base, &thunks.bytes, false);
        map(&mut memory, SCHED, &sched.bytes, false);
        map(&mut memory, TIMER, &[0; 0x100], true);
        let (mut cpu, done) = caller(&mut memory, thunks.thunks[0]);
        cpu.ps |= if depth == 0 { 0 } else { 3 };
        cpu.ar[10] = ticks;
        let mut bus = ClockMemory { memory, updates: 0 };
        let (mut blocked, mut finished) = (false, false);
        for _ in 0..gate.max_insns + 128 {
            if cpu.pc == sched.block { assert_eq!(depth, 0); blocked = true; }
            if blocked && cpu.ps & 15 == 0 {
                assert_ne!(cpu.interrupt & (1 << YIELD_LINE), 0, "R3 block raises yield before unmasking");
                finished = true; break;
            }
            match cpu.step(&mut bus) {
                Ok(()) => {}
                Err(Stop::Break { pc }) => {
                    assert_eq!((pc, ticks, depth), (done, 0, 1), "a non-waiting export still returns inside a section");
                    assert_eq!(cpu.ps & 15, 3);
                    finished = true; break;
                }
                Err(Stop::Exception { cause: 0, pc, .. }) => {
                    assert_eq!((depth, ticks, pc), (1, 1, *fault));
                    assert_eq!(&cpu.ar[2..5], &[u32::from(FAULT_KERNEL_BLOCK_IN_CRITICAL), 0, u32::from(export)]);
                    assert_eq!(cpu.ps & 15, 15, "fatal path never opens the section");
                    let mut record = vec![0; FAULT_WORDS as usize];
                    for (at, value) in [(FAULT_LENGTH, FAULT_WORDS), (FAULT_ISLAND_GUARD, GUARD), (FAULT_ISLAND_STATUS, ISLAND_READY),
                        (FAULT_PC, pc), (FAULT_PS, cpu.ps),
                        (FAULT_WINDOWBASE, cpu.window_base), (FAULT_WINDOWSTART, cpu.window_start)] { record[at as usize] = value; }
                    let physical = cpu.physical_ar();
                    for r in 0..64 { record[FAULT_AR as usize + r] = physical[(4 * cpu.window_base as usize + r) % 64]; }
                    let decoded = RadioFault::decode(&record).unwrap()
                        .with_map(&format!("0x{fault:08x} {fault_size} TEXT/FUNC kernel_fault kernel\n"));
                    assert_eq!(decoded.kernel_fault(), Some(Fault::Kernel { kind: KernelFault::BlockInCritical { export }, site: 0 }));
                    finished = true; break;
                }
                Err(stop) => panic!("critical block: {stop:?}"),
            }
        }
        assert!(finished);
        assert_eq!(blocked, depth == 0);
        assert_eq!(get(&bus.memory, ram, TASK + TCB_STATE), if ticks == 0 { READY } else { BLOCKED });
        assert_eq!(get(&bus.memory, ram, objects.crit_depth.unwrap()), depth);
    }
}

fn rng_word(index: usize) -> u32 { 0x89ab_cdefu32.wrapping_add(index as u32 * 0x0102_0305) }

struct RngMemory { clock: ClockMemory, snapshot: u64, reads: usize }
impl Bus for RngMemory {
    fn read(&mut self, addr: u32, width: Width, kind: Kind, now: u64) -> Result<u32, ()> {
        let value = self.clock.read(addr, width, kind, now)?;
        if addr == RNG_DATA {
            assert_eq!(width, Width::W);
            let word = rng_word(self.reads);
            self.reads += 1;
            Ok(word)
        } else { Ok(value) }
    }
    fn write(&mut self, addr: u32, width: Width, value: u32, now: u64) -> Result<(), ()> {
        if addr == TIMER + 4 {
            // Every snapshot is already past the previous word's spacing:
            // osi_random performs just its final RNG read on each call.
            self.snapshot += u64::from(RNG_SPACING_US + 1) * u64::from(TICKS_PER_US);
            self.clock.memory.write(TIMER + 0x40, Width::W, (self.snapshot >> 32) as u32, now)?;
            self.clock.memory.write(TIMER + 0x44, Width::W, self.snapshot as u32, now)?;
        }
        self.clock.write(addr, width, value, now)
    }
}

#[test]
fn chunked_random_through_real_gate_and_windowed_thunk() {
    const BUFFER: u32 = RAM + 0x1_0001; // deliberately unaligned, outside heap and state
    let module = load_kernel(&compiled().module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let objects = build_kernel(&module, &KernelOptions { base: 0x4010_0000, state: STATE, t0: T0,
        reports: Some(REPORTS), stop_pending: Some(STOP_PENDING), regions: regions() }).unwrap();
    let gate_base = (objects.base + objects.bytes.len() as u32 + 7) & !3;
    let routines = gates(gate_base, &objects, EntryRegs { state: STATE, shared: SHARED });
    let gate = routines.gates.iter().find(|g| g.name == "osi_random").unwrap();
    assert!(!gate.blocks);
    let thunk_base = (gate_base + routines.bytes.len() as u32 + 7) & !3;
    let thunks = radio::exports(thunk_base, &[], &[radio::Gate {
        name: &gate.name, call: gate.address, blocks: gate.blocks,
        exit_level: None, export: objects.entries.iter().position(|e| e.name == gate.name).unwrap().try_into().unwrap(),
    }], None, None);
    let built = adapter(BASE, |target| if target == gate.name.as_str() { thunks.thunks[0] } else { 0 }, |_| 0);
    let index = OSI_FIELDS.iter().position(|&(field, _)| field == "_get_random").unwrap();
    let symbol = built.symbols.iter().find(|s| s.0 == "osi_chunked__get_random").unwrap();
    assert_eq!(built.table[index], symbol.1, "table points to the unmasked chunk loop");
    let mut offset = (symbol.1 - BASE) as usize;
    let end = offset + symbol.2 as usize;
    let mut static_insns = 0;
    while offset < end {
        let mut bytes = [0; 4];
        let n = (end - offset).min(3);
        bytes[..n].copy_from_slice(&built.bytes[offset..offset + n]);
        let (_, size) = decode(u32::from_le_bytes(bytes)).expect("chunked stub decodes");
        offset += size as usize;
        static_insns += 1;
    }
    eprintln!("Chunked _get_random: {static_insns} static instructions; own execution: zero length 5, nonzero fixed 4, full word 21, tail r=1..3 costs 5*r+2; callees excluded");
    for len in [0usize, 1, 3, 4, 5, 37] {
        for level in [0u32, 3] {
            let mut memory = Memory::new();
            let ram = kernel_ram(&mut memory, &module, &objects);
            map(&mut memory, objects.base, &objects.bytes, false);
            map(&mut memory, gate_base, &routines.bytes, false);
            map(&mut memory, thunk_base, &thunks.bytes, false);
            map(&mut memory, BASE, &built.bytes, false);
            map(&mut memory, TIMER, &[0; 0x100], true);
            map(&mut memory, RNG_DATA, &[0; 4], false);
            let start = (BUFFER - RAM) as usize;
            memory.bytes_mut(ram)[start - 1..start + 48].fill(0xa5);
            let (mut cpu, done) = caller(&mut memory, built.table[index]);
            cpu.ps |= level;
            cpu.ar[10] = if len == 0 { 0 } else { BUFFER }; // zero length must not touch even NULL
            cpu.ar[11] = len as u32;
            let mut bus = RngMemory { clock: ClockMemory { memory, updates: 0 }, snapshot: 0, reads: 0 };
            let words = len.div_ceil(4);
            let mut calls = 0;
            let mut own_insns = 0;
            let mut stopped = false;
            for _ in 0..words as u64 * (gate.max_insns + 128) + 128 {
                if cpu.pc == thunks.thunks[0] {
                    assert_eq!(cpu.ps & 15, level, "len {len}: caller INTLEVEL before word {calls}");
                    calls += 1;
                }
                if (symbol.1..symbol.1 + symbol.2).contains(&cpu.pc) {
                    assert_eq!(cpu.ps & 15, level, "len {len}: INTLEVEL restored throughout the chunk loop");
                    own_insns += 1;
                }
                match cpu.step(&mut bus) {
                    Ok(()) => {}
                    Err(Stop::Break { pc }) => { assert_eq!(pc, done); stopped = true; break; }
                    Err(stop) => panic!("Chunked len {len}, level {level}: {stop:?}"),
                }
            }
            assert!(stopped, "len {len}: chunk loop returns within its composed bound");
            assert_eq!(cpu.ar[10], 0, "get_random returns success");
            assert_eq!(cpu.ps & 15, level, "caller's interrupt level restored on return");
            assert_eq!(cpu.window_base, 15, "caller's register window restored");
            assert_eq!(calls, words, "exactly ceil(len / 4) thunk calls");
            assert_eq!(bus.reads, words, "one spaced RNG read per masked call");
            assert_eq!(bus.clock.updates, words, "one snapshot per word");
            let expected: Vec<_> = (0..words).flat_map(|i| rng_word(i).to_le_bytes()).take(len).collect();
            assert_eq!(&bus.clock.memory.bytes(ram)[start..start + len], expected);
            assert_eq!(bus.clock.memory.bytes(ram)[start - 1], 0xa5, "byte before output untouched");
            assert!(bus.clock.memory.bytes(ram)[start + len..start + 48].iter().all(|&v| v == 0xa5),
                "len {len}: every byte after output untouched");
            let tail = len % 4;
            let expected_own = if len == 0 { 5 } else { 4 + 21 * (len / 4) + if tail == 0 { 0 } else { 5 * tail + 2 } };
            assert_eq!(own_insns, expected_own, "len {len}: documented stub instruction cost");
        }
    }
}

#[test]
fn named_stub_reaches_r3_fault_record() {
    use radio::*;
    const CURRENT: u32 = RAM;
    const TASK: u32 = RAM + 0x200;
    const FAULT: u32 = RAM + 0x400;
    const ISLAND_GUARD_ADDR: u32 = RAM + 28;
    const ISLAND_STATUS_ADDR: u32 = RAM + 32;
    const SWITCH: u32 = VECTORS + 0x8000;
    let kernel = kernel_code(SWITCH, VECTORS, &RadioLayout {
        current: CURRENT, irq_stack_base: RAM + 0x20000, irq_stack_top: RAM + 0x22000,
        pick: SWITCH + 0x7000, tasks: RAM + 0x100, task_count: RAM + 8,
        dispatcher: RAM + 0x180, idle: RAM + 0x1c0, isr_count: RAM + 12, wake_lines: 0, vector_lines: 0,
        shim_start: SWITCH + 0x7100, shim_end: SWITCH + 0x7200,
        radio_state: RAM + 16, island_guard: ISLAND_GUARD_ADDR, island_status: ISLAND_STATUS_ADDR,
        fault: FAULT, trace_head: RAM + 20, trace: RAM + 24, trace_entries: 0,
    });
    let built = adapter(BASE, |_| 0, |_| 0);
    for field in ["_is_from_isr", "_nvs_erase_key", "aes_wrap"] {
        let index = OSI_FIELDS.iter().chain(&CRYPTO_FIELDS).position(|&(name, _)| name == field).unwrap();
        let entry = built.table.iter().chain(&built.crypto).nth(index).copied().unwrap();
        // Named fault rows through the real exception recorder.
        let mut memory = Memory::new();
        let ram = map(&mut memory, RAM, &vec![0; RAM_BYTES], true);
        for (at, value) in [(CURRENT, TASK), (RAM + 8, 1), (RAM + 0x100, TASK),
            (TASK + TCB_STACK_BASE, RAM + 0x24000), (TASK + TCB_STACK_TOP, RAM + RAM_BYTES as u32),
            (TASK + TCB_STATE, READY), (RAM + 16, RADIO_RUNNING), (ISLAND_GUARD_ADDR, GUARD), (ISLAND_STATUS_ADDR, ISLAND_READY)] {
            put(&mut memory, ram, at, value);
        }
        let mut vectors = vec![0; 0x400];
        vectors[..WINDOW_BLOCK_SIZE].copy_from_slice(&window_block());
        let user = user_vector(VECTORS, kernel.irq_entry, kernel.fault_entry);
        vectors[USER_VECTOR_OFFSET..USER_VECTOR_OFFSET + user.len()].copy_from_slice(&user);
        map(&mut memory, VECTORS, &vectors, false);
        map(&mut memory, SWITCH, &kernel.bytes, false);
        map(&mut memory, BASE, &built.bytes, false);
        let (mut cpu, _) = caller(&mut memory, entry);
        cpu.traps = TrapMode::Take;
        let mut reached = false;
        for _ in 0..2000 {
            if get(&memory, ram, FAULT + FAULT_LENGTH * 4) == FAULT_WORDS { reached = true; break; }
            assert_ne!(cpu.pc, kernel.fatal, "island fault must be recoverable");
            cpu.step(&mut memory).unwrap();
        }
        assert!(reached, "R3 published the complete island fault frame");
        assert_eq!(get(&memory, ram, FAULT + FAULT_CAUSE * 4), 0);
        assert_eq!(get(&memory, ram, FAULT + (FAULT_AR + 2) * 4), index as u32);
        let pc = get(&memory, ram, FAULT + FAULT_PC * 4);
        assert!(built.symbols.iter().any(|s| s.1 <= pc && pc < s.1 + s.2 && s.0.ends_with(field)));
    }
}

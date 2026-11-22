//! Kernel object contracts on device-addressed host memory. No hardware or
//! wall clock: the caller chooses CURRENT and advances KernelPorts::clock.
use fab_elec_mcu::{compile_kernel, Compiled};
use fab_elec_mcu_emit_s3::{build_kernel, chip, radio::{self, task::*, MIN_STACK_BYTES, PACK_ISR_LINES,
    sched::{READY, BLOCKED, DEAD, YIELD_LINE, CURSOR_OFFSET, ISR_SLOT},
    kernel::{kernel_consts, KernelLayout, OBJECTS, TICKS_PER_US, BLOB_TICK_US, CRIT_MASK,
        SYSCON_WIFI_RST_EN, WIFIMAC_RST, EFUSE_MAC_0, EFUSE_MAC_1, RTC_STORE1,
        RNG_DATA, RNG_SPACING_US, RNG_POLL_CAP, INTR_MATRIX, WIFI_SOURCES, LEVEL1_EXTERNAL_LINES, ROUTABLE_LINES,
        SYSCON_WIFI_CLK_EN, CLK_COMMON, CLK_BT_BASEBAND, CLK_RNG, PHY_MODEM_WIFI, PHY_RF_CAL_FULL,
        ESP_CAL_DATA_CHECK_FAIL, PHY_TRACK_PLL_PERIOD_US, PHY_SKIP, PHY_ON, PHY_FIRST, PHY_WAKE,
        PHY_WAKE_RESTORE, PHY_LAST, PHY_NOT_LAST, PHY_OP_ENABLE, PHY_OP_DISABLE},
    adapter::BLOB_MAX_PRIORITY}, KernelOptions, KernelRegion, RADIO_KERNEL_LINES};
use fab_elec_mcu_rt::{self as rt, kernel::{call_export, KernelMemory, KernelPorts, RegionKind},
    exec::{PortCall, Ports},
    module::{load_kernel, Module}, sched::{Fault, Hal, Role}, watch::{FAULT_KERNEL_IRQ_LINE_REFUSED,
        FAULT_KERNEL_IRQ_SOURCE_REFUSED, FAULT_KERNEL_ISR_TABLE_FULL, FAULT_KERNEL_ISR_ENTRY_REFUSED,
        FAULT_KERNEL_CRIT_MISMATCH, FAULT_KERNEL_CRIT_SPAN, FAULT_KERNEL_TIMER_REFUSED,
        FAULT_KERNEL_TIMER_HANDLE_INVALID, FAULT_KERNEL_TIMER_LOST, FAULT_KERNEL_TIMER_UNSET,
        FAULT_KERNEL_HEAP_EXHAUSTED, FAULT_RADIO_PHY_INIT_FAILED, FAULT_RADIO_START_FAILED}};
use std::collections::VecDeque;

#[path = "support/heap_check.rs"] mod heap_check;

#[derive(Default)]
struct Host { accept_reports: bool, reports: Vec<(u64, u8, i32)> }
impl Hal for Host {
    fn configure(&mut self, _: u64, _: u32, _: Role) { unreachable!() }
    fn serial(&mut self, _: u64, _: u32, _: rt::serial::SerialSetting) { unreachable!() }
    fn write(&mut self, _: u64, _: u32, _: bool, _: u32) { unreachable!() }
    fn shift(&mut self, _: u64, _: u32, _: u32, _: u32, _: u8, _: u32) { unreachable!() }
    fn duty(&mut self, _: u64, _: u32, _: u8, _: u32) { unreachable!() }
    fn read(&mut self, _: u64, _: u32, _: u32) -> bool { unreachable!() }
    fn adc(&mut self, _: u64, _: u32, _: u32) -> u16 { unreachable!() }
    fn eth_send(&mut self, _: u64, _: &[u8], _: u32) -> bool { unreachable!() }
    fn eth_mac(&mut self, _: u64, _: u32) -> [u8; 6] { unreachable!() }
    fn eeprom_read(&mut self, _: u64, _: u16, _: u32) -> u8 { unreachable!() }
    fn eeprom_write(&mut self, _: u64, _: u16, _: u8, _: u32) { unreachable!() }
    fn clock(&mut self, t: u64, _: u32) -> u64 { t }
    fn fault(&mut self, tick: u64, fault: Fault) {
        if self.accept_reports {
            if let Fault::KernelReport { code, arg, .. } = fault { self.reports.push((tick, code, arg)); return; }
        }
        panic!("kernel object fault: {fault:?}")
    }
}

struct Run<'a> { module: Module<'a>, state: Vec<u32>, scratch: Vec<u32> }
impl<'a> Run<'a> {
    fn new(c: &'a Compiled) -> Self {
        let module = load_kernel(&c.module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
        let mut state = module.state_init().to_vec();
        for var in module.vars() {
            match module.string(var.name) {
                "task_exit_link" => state[var.offset as usize] = windowed_exit_link(TASK_EXIT),
                "phy_init_data" => state[var.offset as usize] = PHY_INIT,
                _ => {}
            }
        }
        let scratch = vec![0; (0..module.entries().len()).map(|i| module.scratch_words(i)).max().unwrap_or(0)];
        Self { module, state, scratch }
    }
    fn call(&mut self, name: &str, args: &[i32], io: &mut dyn Ports) -> i32 {
        call_export(&self.module, name, args, &mut self.state, &mut self.scratch, io)
            .unwrap_or_else(|e| panic!("{name}{args:?}: {e:?}"))
    }
}

// The first test gates the objects' use of bare injected constants. Keep it
// independent of OBJECTS so a front-end regression is immediately legible.
#[test]
fn a_layout_constants_fold_without_state() {
    let source = "let value = export(fn() { return TCB_STATE })";
    let p = chip::profile();
    let plain = compile_kernel("let value = export(fn() { return 12 })", &p, &RADIO_KERNEL_LINES, &[]).unwrap();
    let used = compile_kernel(source, &p, &RADIO_KERNEL_LINES, &[("TCB_STATE", 12)]).unwrap();
    let extra = compile_kernel(source, &p, &RADIO_KERNEL_LINES,
        &[("TCB_STATE", 12), ("UNUSED_LAYOUT", i32::MIN)]).unwrap();
    let plain = Run::new(&plain);
    let mut used = Run::new(&used);
    let extra = Run::new(&extra);
    for run in [&used, &extra] {
        assert!(!run.module.vars().iter().any(|v| matches!(run.module.string(v.name), "TCB_STATE" | "UNUSED_LAYOUT")),
            "injected constants must not become state variables");
        assert_eq!(run.module.state_words(), plain.module.state_words(), "used and unused constants allocate no state");
    }
    let mut host = Host::default();
    let mut io = KernelPorts::new(KernelMemory::new(), RADIO_KERNEL_LINES, &mut host);
    assert_eq!(used.call("value", &[], &mut io), 12, "export reads the injected value");
    for item in ["let TCB_STATE = 9", "var TCB_STATE = 9", "fn TCB_STATE() { 9 }",
        "struct TCB_STATE { x: 9 }", "let TCB_STATE = export(fn() { return 9 })"] {
        let source = format!("{item}\nlet entry = export(fn() {{ return 0 }})");
        let errors = match compile_kernel(&source, &p, &RADIO_KERNEL_LINES, &[("TCB_STATE", 12)]) {
            Err(e) => e, Ok(_) => panic!("must refuse a layout collision: {item}"),
        };
        assert!(errors.iter().any(|e| e.message.contains("TCB_STATE") && e.message.contains("redeclared")),
            "collision must name the layout constant: {errors:?}");
    }
}

const HEAP: u32 = 0x4000;
const HEAP_BYTES: u32 = 2048;
const CURRENT: u32 = 0x1000;
const TASKS: u32 = 0x1100;
const TASK_COUNT: u32 = TASKS + TASKS_MAX * 4;
const TASKS_MAX: u32 = 6;
const ISRS: u32 = 0x1200;
const ISR_COUNT: u32 = 0x1240;
const SHIM: u32 = 0x4000_0180;
const PHY_INIT: u32 = 0x5000;
const PHY_CAL: u32 = 0x5100;
const PHY_REGS: u32 = 0x5900;
const PHY_MUTEX: u32 = 0x5960;
// First guesses for the radio layout; size these from measurements in R9.
const PLANNED_HEAP_BYTES: u32 = 128 * 1024;
const PLANNED_MAX_TASKS: u32 = 16;
const TCBS: u32 = 0x2000;
const BUFFERS: u32 = 0x3000;
const DEADLINE_OUT: u32 = BUFFERS + 0x700;
const WOKEN: u32 = BUFFERS + 0x711; // deliberately not word aligned
const REPORTS: u32 = BUFFERS + 0x600;
// Immediately after the 3-word header and 8 three-word report entries.
const STOP_PENDING: u32 = REPORTS + 12 + radio::REPORT_RING_ENTRIES * 12;
const TASK_EXIT: u32 = 0x4000_0100;
const PRIORITIES: [u32; TASKS_MAX as usize] = [1, 2, 0x8000_0000, 0x8000_0000, 3, 1];
const EXPORTS: &[&str] = &[
    "malloc", "free", "calloc_reserve", "heap_request_bytes", "realloc_reserve", "heap_free_bytes", "heap_failures",
    "kernel_expire", "queue_create", "queue_delete", "queue_send_back", "queue_send_front", "queue_recv",
    "queue_send_back_isr", "queue_send_front_isr", "queue_recv_isr", "queue_waiting",
    "sem_create", "sem_delete", "sem_take", "sem_give", "sem_take_isr", "sem_give_isr", "sem_count",
    "mutex_create", "mutex_delete", "mutex_lock", "mutex_unlock",
    "task_create", "task_delete", "task_reap", "task_delay", "task_current",
    "timer_setfn", "timer_arm_us", "timer_arm", "timer_disarm", "timer_done", "timer_due", "timer_failures", "timer_invalid",
    "timer_lost", "timer_unset", "timer_slots", "timer_high_water",
    "osi_spin_lock_create", "osi_semphr_take", "osi_wifi_thread_semphr_get",
    "osi_mutex_create", "osi_recursive_mutex_create", "osi_mutex_lock",
    "osi_queue_send_to_back", "osi_queue_send_to_front", "osi_queue_recv", "osi_queue_send_from_isr",
    "osi_wifi_create_queue", "osi_wifi_delete_queue", "osi_task_create", "osi_task_delete",
    "osi_task_delay", "osi_task_ms_to_tick", "osi_esp_timer_get_time", "osi_get_time",
    "osi_log_timestamp", "osi_ints_on", "osi_ints_off", "osi_wifi_reset_mac",
    "osi_read_mac", "osi_slowclk_cal_get", "osi_random",
    "osi_set_intr", "osi_set_isr", "osi_wifi_int_disable", "osi_wifi_int_restore",
    "phy_lock", "phy_enter", "phy_init_done", "phy_leave",
    "puts", "net80211_printf", "pp_printf", "phy_printf", "print_dropped", "event_commit", "event_take", "radio_start_failed",
];

fn layout() -> KernelLayout {
    KernelLayout { current: CURRENT, tasks: TASKS, task_count: TASK_COUNT, max_tasks: TASKS_MAX,
        phy_cal_data: PHY_CAL, phy_dig_regs: PHY_REGS, phy_mutex: PHY_MUTEX,
        isrs: ISRS, isr_count: ISR_COUNT, isr_capacity: PACK_ISR_LINES, code_region: TASK_EXIT >> 30,
        heap_base: HEAP, heap_size: HEAP_BYTES, timer_reserve: 0 }
}
fn compiled_objects() -> &'static Compiled {
    static SOURCE: std::sync::OnceLock<Compiled> = std::sync::OnceLock::new();
    SOURCE.get_or_init(|| compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES, &kernel_consts(&layout()))
        .unwrap_or_else(|errors| panic!("objects.splash: {errors:?}")))
}
fn regions() -> Vec<KernelRegion> {
    vec![
        KernelRegion { base: HEAP, len: HEAP_BYTES, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: TCBS, len: TASKS_MAX * TCB_SIZE as u32, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: CURRENT, len: CURSOR_OFFSET + 4, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: TASKS, len: (TASKS_MAX + 1) * 4, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: BUFFERS, len: 2048, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: ISRS, len: ISR_COUNT - ISRS + 4, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: PHY_INIT, len: 128, writable: false, kind: RegionKind::Pack },
        KernelRegion { base: PHY_CAL, len: 1904, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: PHY_REGS, len: 84, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: PHY_MUTEX, len: radio::kernel::MUTEX_BYTES, writable: true, kind: RegionKind::Kernel },
    ].into_iter().chain(device_regions()).collect()
}
fn device_regions() -> [KernelRegion; 6] {
    [
        KernelRegion { base: SYSCON_WIFI_RST_EN, len: 4, writable: true, kind: RegionKind::Device },
        KernelRegion { base: SYSCON_WIFI_CLK_EN, len: 4, writable: true, kind: RegionKind::Device },
        KernelRegion { base: EFUSE_MAC_0, len: EFUSE_MAC_1 - EFUSE_MAC_0 + 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: RTC_STORE1, len: 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: RNG_DATA, len: 4, writable: false, kind: RegionKind::Device },
        KernelRegion { base: INTR_MATRIX, len: WIFI_SOURCES * 4, writable: true, kind: RegionKind::Device },
    ]
}
fn tcb(task: u32) -> u32 { TCBS + task * TCB_SIZE as u32 }
fn buffer(task: u32) -> i32 { (BUFFERS + task * 128 + 1) as i32 }
fn get(memory: &KernelMemory, at: u32) -> u32 {
    u32::from_le_bytes(memory.bytes(at, 4).unwrap().try_into().unwrap())
}
fn put(memory: &mut KernelMemory, at: u32, value: u32) {
    memory.bytes_mut(at, 4).unwrap().copy_from_slice(&value.to_le_bytes());
}
fn get64(memory: &KernelMemory, at: u32) -> u64 {
    u64::from_le_bytes(memory.bytes(at, 8).unwrap().try_into().unwrap())
}
fn heap_status(memory: &KernelMemory) -> u32 {
    heap_status_for_layout(memory, &layout())
}
fn heap_status_for_layout(memory: &KernelMemory, layout: &KernelLayout) -> u32 {
    heap_check::heap_check(|at| get(memory, at), |at| memory.bytes(at, 1).unwrap()[0], layout.heap_base, layout.heap_size)
}
fn memory() -> KernelMemory {
    let mut memory = KernelMemory::new();
    for region in regions() {
        let mut bytes = vec![0; region.len as usize];
        // Seed device state before making the read-only regions immutable.
        if region.base == EFUSE_MAC_0 {
            bytes[..4].copy_from_slice(&0x89ab_cdffu32.to_le_bytes());
            bytes[4..].copy_from_slice(&0xbeef_0234u32.to_le_bytes());
        } else if region.base == RTC_STORE1 {
            bytes.copy_from_slice(&0xfedc_ba98u32.to_le_bytes());
        }
        memory.add_region(region.base, bytes, region.writable, region.kind).unwrap();
    }
    put(&mut memory, CURRENT, tcb(0));
    put(&mut memory, TASK_COUNT, TASKS_MAX);
    for (task, priority) in PRIORITIES.into_iter().enumerate() {
        let task = task as u32;
        put(&mut memory, TASKS + task * 4, tcb(task));
        put(&mut memory, tcb(task) + TCB_PRIORITY, priority);
        put(&mut memory, tcb(task) + TCB_STATE, READY);
    }
    assert!(memory.bytes(HEAP, HEAP_BYTES as usize).unwrap().iter().all(|&b| b == 0),
        "kernel_objects memory must supply the zeroed heap required by objects.splash");
    memory
}
fn call(run: &mut Run<'_>, io: &mut KernelPorts<'_>, task: u32, name: &str, args: &[i32]) -> i32 {
    put(&mut io.memory, CURRENT, tcb(task));
    io.raised.clear();
    run.call(name, args, io)
}
fn completed(io: &KernelPorts<'_>, task: u32, result: u32, scenario: &str) {
    assert_eq!(get(&io.memory, tcb(task) + TCB_STATE), READY, "{scenario}: task {task} ready");
    assert_eq!(get(&io.memory, tcb(task) + TCB_RESULT), result, "{scenario}: task {task} result");
    assert_eq!(get(&io.memory, tcb(task) + TCB_WAIT_OBJ), 0, "{scenario}: task {task} detached");
    assert_eq!(get(&io.memory, tcb(task) + TCB_WAIT_NEXT), 0, "{scenario}: task {task} next cleared");
    assert_eq!(get(&io.memory, tcb(task) + TCB_WAIT_PREV), 0, "{scenario}: task {task} prev cleared");
}
fn waiters(io: &KernelPorts<'_>, tasks: &[u32], scenario: &str) {
    if let Some(&first) = tasks.first() {
        let head = get(&io.memory, tcb(first) + TCB_WAIT_OBJ) & !1;
        assert_ne!(head, 0, "{scenario}: wait identity");
        assert_eq!(get(&io.memory, head), tcb(first), "{scenario}: first waiter");
        let mut link = head;
        for (index, &task) in tasks.iter().enumerate() {
            assert_eq!(get(&io.memory, tcb(task) + TCB_STATE), BLOCKED, "{scenario}: task {task} blocked");
            assert_eq!(get(&io.memory, tcb(task) + TCB_WAIT_OBJ) & !1, head, "{scenario}: same wait list");
            assert_eq!(get(&io.memory, tcb(task) + TCB_WAIT_NEXT), tasks.get(index + 1).map_or(0, |&t| tcb(t)),
                "{scenario}: priority/FIFO successor of task {task}");
            assert_eq!(get(&io.memory, tcb(task) + TCB_WAIT_PREV), link, "{scenario}: back link of task {task}");
            link = tcb(task) + TCB_WAIT_NEXT;
        }
    }
}

#[test]
fn layout_bindings_and_address_refusals() {
    let pairs = kernel_consts(&layout());
    let expected = [
        ("TCB_STATE", TCB_STATE), ("TCB_PRIORITY", TCB_PRIORITY), ("TCB_DEADLINE", TCB_DEADLINE),
        ("TCB_RESULT", TCB_RESULT), ("TCB_WAIT_NEXT", TCB_WAIT_NEXT), ("TCB_WAIT_PREV", TCB_WAIT_PREV),
        ("TCB_WAIT_OBJ", TCB_WAIT_OBJ),
        ("TCB_WAIT_ARG", TCB_WAIT_ARG), ("READY", READY), ("BLOCKED", BLOCKED), ("YIELD_LINE", YIELD_LINE),
        ("TICKS_PER_US", TICKS_PER_US), ("TICKS_HIGH_SHIFT", 32 - TICKS_PER_US.trailing_zeros()),
        ("BLOB_TICK_US", BLOB_TICK_US), ("CRIT_MASK", CRIT_MASK),
        ("FAULT_KERNEL_CRIT_MISMATCH", FAULT_KERNEL_CRIT_MISMATCH as u32),
        ("FAULT_KERNEL_CRIT_SPAN", FAULT_KERNEL_CRIT_SPAN as u32),
        ("FAULT_KERNEL_TIMER_REFUSED", FAULT_KERNEL_TIMER_REFUSED as u32),
        ("FAULT_KERNEL_TIMER_LOST", FAULT_KERNEL_TIMER_LOST as u32),
        ("FAULT_KERNEL_TIMER_UNSET", FAULT_KERNEL_TIMER_UNSET as u32),
        ("FAULT_KERNEL_HEAP_EXHAUSTED", FAULT_KERNEL_HEAP_EXHAUSTED as u32),
        ("FAULT_KERNEL_TIMER_HANDLE_INVALID", FAULT_KERNEL_TIMER_HANDLE_INVALID as u32),
        ("BLOB_MAX_PRIORITY", BLOB_MAX_PRIORITY as u32),
        ("SYSCON_WIFI_RST_EN", 0x6002_6018), ("WIFIMAC_RST", 1 << 2),
        ("EFUSE_MAC_0", 0x6000_7044), ("EFUSE_MAC_1", 0x6000_7048),
        ("RTC_STORE1", 0x6000_8054), ("RNG_DATA", 0x6003_507c),
        ("RNG_SPACING_US", 2), ("RNG_POLL_CAP", RNG_POLL_CAP),
        ("INTR_MATRIX", 0x600c_2000), ("WIFI_SOURCES", 4), ("ROUTABLE_LINES", 0x0006_3739),
        ("FAULT_KERNEL_IRQ_LINE_REFUSED", FAULT_KERNEL_IRQ_LINE_REFUSED as u32),
        ("FAULT_KERNEL_IRQ_SOURCE_REFUSED", FAULT_KERNEL_IRQ_SOURCE_REFUSED as u32),
        ("FAULT_KERNEL_ISR_TABLE_FULL", FAULT_KERNEL_ISR_TABLE_FULL as u32),
        ("FAULT_KERNEL_ISR_ENTRY_REFUSED", FAULT_KERNEL_ISR_ENTRY_REFUSED as u32),
        ("ISRS", ISRS), ("ISR_COUNT", ISR_COUNT), ("ISR_CAPACITY", PACK_ISR_LINES),
        ("ISR_SLOT", ISR_SLOT), ("CODE_REGION", SHIM >> 30),
        ("CURRENT", CURRENT), ("TASKS", TASKS), ("TASK_COUNT", TASK_COUNT),
        ("MAX_TASKS", TASKS_MAX), ("HEAP_BASE", HEAP), ("HEAP_SIZE", HEAP_BYTES),
        ("TIMER_FLOOR", 0), ("H_SL", 68), ("H_TABLES", 320), ("H_DIR", 320),
        ("TIMER_DIR_WORDS", 1), ("TIMER_SIFT_CAP", 6), ("T_BYTES", 36),
        ("H_FL_COUNT", HEAP_BYTES.ilog2() - 4), ("H_SL_COUNT", 8),
        ("H_FIXED", radio::kernel::heap_control_bytes(HEAP_BYTES)), ("M_BYTES", radio::kernel::MUTEX_BYTES),
        ("CURSOR", CURRENT + CURSOR_OFFSET), ("TCB_FRAME", TCB_FRAME),
        ("TCB_STACK_BASE", TCB_STACK_BASE), ("TCB_STACK_TOP", TCB_STACK_TOP),
        ("TCB_NAME", TCB_NAME), ("TCB_SIZE", TCB_SIZE as u32), ("DEAD", DEAD),
        ("TCB_THREAD_SEM", TCB_THREAD_SEM),
        ("CONTEXT_SIZE", CONTEXT_SIZE as u32), ("CONTEXT_PC", CONTEXT_PC), ("CONTEXT_PS", CONTEXT_PS),
        ("CONTEXT_AR", CONTEXT_AR), ("CONTEXT_CPENABLE", CONTEXT_CPENABLE),
        ("MIN_STACK_BYTES", MIN_STACK_BYTES),
        ("SYSCON_WIFI_CLK_EN", SYSCON_WIFI_CLK_EN), ("CLK_COMMON", CLK_COMMON),
        ("CLK_BT_BASEBAND", CLK_BT_BASEBAND), ("CLK_RNG", CLK_RNG),
        ("PHY_MODEM_WIFI", PHY_MODEM_WIFI), ("PHY_RF_CAL_FULL", PHY_RF_CAL_FULL),
        ("ESP_CAL_DATA_CHECK_FAIL", ESP_CAL_DATA_CHECK_FAIL),
        ("PHY_TRACK_PLL_PERIOD_US", PHY_TRACK_PLL_PERIOD_US),
        ("PHY_SKIP", PHY_SKIP), ("PHY_ON", PHY_ON), ("PHY_FIRST", PHY_FIRST),
        ("PHY_WAKE", PHY_WAKE), ("PHY_WAKE_RESTORE", PHY_WAKE_RESTORE),
        ("PHY_LAST", PHY_LAST), ("PHY_NOT_LAST", PHY_NOT_LAST),
        ("PHY_OP_ENABLE", PHY_OP_ENABLE), ("PHY_OP_DISABLE", PHY_OP_DISABLE),
        ("PHY_CAL_DATA", PHY_CAL), ("PHY_DIG_REGS", PHY_REGS),
        ("PHY_MUTEX", PHY_MUTEX),
        ("FAULT_KERNEL_PHY_INIT_FAILED", FAULT_RADIO_PHY_INIT_FAILED as u32),
        ("FAULT_RADIO_START_FAILED", FAULT_RADIO_START_FAILED as u32),
        ("GUARD", GUARD), ("WINDOWED_PS", WINDOWED_PS),
    ];
    assert_eq!(pairs, expected.map(|(n, v)| (n, v as i32)));
    assert_eq!(TICKS_PER_US, 16, "SYSTIMER microsecond conversion");
    assert_eq!(ROUTABLE_LINES, LEVEL1_EXTERNAL_LINES & !RADIO_KERNEL_LINES.protected);
    assert_eq!(TCB_SIZE, 56, "per-task semaphore plus eight-byte allocation alignment");
    assert!(std::panic::catch_unwind(|| kernel_consts(&KernelLayout { code_region: 4, ..layout() })).is_err(),
        "windowed code region must fit bits 31:30");
    for field in ["current", "tasks", "task_count", "heap_base", "isrs", "isr_count", "cursor overflow"] {
        let mut l = layout();
        match field { "current" => l.current = 0x8000_0000, "tasks" => l.tasks = 0x8000_0000,
            "task_count" => l.task_count = 0x8000_0000, "heap_base" => l.heap_base = 0x8000_0000,
            "isrs" => l.isrs = 0x8000_0000, "isr_count" => l.isr_count = 0x8000_0000,
            _ => l.current = u32::MAX - CURSOR_OFFSET + 1 }
        assert!(std::panic::catch_unwind(|| kernel_consts(&l)).is_err(), "{field}: address must fit i32");
    }
}

fn create_task(run: &mut Run<'_>, io: &mut KernelPorts<'_>, bytes: u32, priority: u32) -> u32 {
    const ENTRY: u32 = 0x4000_2000;
    const ARG: u32 = 0xfedc_ba98;
    let name = BUFFERS + 64;
    assert_eq!(call(run, io, 0, "task_create",
        &[ENTRY as i32, name as i32, bytes as i32, ARG as i32, priority as i32, BUFFERS as i32]), 1);
    assert_eq!(io.raised, if priority > PRIORITIES[0] { vec![YIELD_LINE] } else { vec![] });
    let task = get(&io.memory, BUFFERS);
    assert!((HEAP..HEAP + HEAP_BYTES).contains(&task), "TCB is heap-owned");
    assert_eq!(task & 7, 0);
    let base = get(&io.memory, task + TCB_STACK_BASE);
    let top = get(&io.memory, task + TCB_STACK_TOP);
    assert_eq!(top & 15, 0, "ABI stack alignment");
    assert!(top - base >= bytes.max(MIN_STACK_BYTES), "requested stack capacity");
    assert_eq!(get(&io.memory, base), GUARD, "new stack guard");
    let frame = initial_frame(base, top, ENTRY, TASK_EXIT, TaskAbi::Windowed, [ARG, 0, 0, 0, 0, 0]);
    assert_eq!(get(&io.memory, task + TCB_FRAME), frame.address);
    assert_eq!(get(&io.memory, frame.address + CONTEXT_AR + 8 * 4), 0x8000_0100, "windowed exit link retains bit 31");
    assert_eq!(io.memory.bytes(frame.address, CONTEXT_SIZE).unwrap(), frame.bytes, "every initial context byte");
    for (offset, value) in [(TCB_STATE, READY), (TCB_PRIORITY, priority), (TCB_NAME, name),
        (TCB_DEADLINE, 0), (TCB_DEADLINE + 4, 0), (TCB_RESULT, 0),
        (TCB_WAIT_NEXT, 0), (TCB_WAIT_OBJ, 0), (TCB_WAIT_ARG, 0), (TCB_THREAD_SEM, 0), (TCB_WAIT_PREV, 0)] {
        assert_eq!(get(&io.memory, task + offset), value, "new TCB field {offset}");
    }
    task
}

#[test]
fn task_capacity_and_allocation_failures_table() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    put(&mut io.memory, TASK_COUNT, 4);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    for (scenario, entry, bytes, priority) in [
        ("idle priority", 0x4000_2000, 0, 0),
        ("wrong RETW region", 0x2000, 0, 1),
        ("oversize stack", 0x4000_2000, -1, 1),
        ("stack allocation fails after TCB allocation", 0x4000_2000, initial, 1),
    ] {
        put(&mut io.memory, BUFFERS, 0xdead_beef);
        assert_eq!(call(&mut run, &mut io, 0, "task_create",
            &[entry, 0, bytes, 0, priority, BUFFERS as i32]), 0, "{scenario}");
        assert_eq!(get(&io.memory, BUFFERS), 0xdead_beef, "{scenario}: no published handle");
        assert_eq!(get(&io.memory, TASK_COUNT), 4, "{scenario}: no table entry");
        assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial, "{scenario}: no leak");
    }
    let mut live = Vec::new();
    for index in 4..TASKS_MAX {
        let task = create_task(&mut run, &mut io, 337, 1);
        assert_eq!(get(&io.memory, TASKS + index * 4), task, "append in order");
        live.push(task);
    }
    let full = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    let failures = call(&mut run, &mut io, 0, "heap_failures", &[]);
    assert_eq!(call(&mut run, &mut io, 0, "task_create", &[0x4000_2000, 0, 0, 0, 1, 0]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "heap_failures", &[]), failures, "capacity refusal, not heap exhaustion");
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), full, "capacity refusal frees both allocations");
    for task in live { assert_eq!(call(&mut run, &mut io, 0, "task_delete", &[task as i32]), 1); }
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    assert_eq!(call(&mut run, &mut io, 0, "task_create", &[0x4000_2000, 0, 0, 0, 1, 0]), 1, "null handle output");
    let task = get(&io.memory, TASKS + 4 * 4);
    assert_eq!(call(&mut run, &mut io, 0, "task_delete", &[task as i32]), 1);
    for (role, id) in [("dispatcher", 0), ("idle", 1), ("boot", 2)] {
        if role == "idle" { put(&mut io.memory, tcb(id) + TCB_PRIORITY, 0); }
        for handle in [tcb(id) as i32, 0] {
            assert_eq!(call(&mut run, &mut io, id, "task_delete", &[handle]), 0, "refuse {role}");
            assert_eq!(get(&io.memory, tcb(id) + TCB_STATE), READY, "{role} unchanged");
        }
    }
    assert_eq!(call(&mut run, &mut io, 0, "task_delete", &[HEAP as i32]), 0, "heap address absent from TASKS");
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    check_heap(&io, 0, 0);
    drop(io);
    assert_eq!(host.reports, [(0, FAULT_KERNEL_HEAP_EXHAUSTED, -1), (0, FAULT_KERNEL_HEAP_EXHAUSTED, initial + 15)]);
}

#[test]
fn task_delete_waiter_shift_cursor_and_reap_table() {
    let mut run = Run::new(compiled_objects());
    for (scenario, removed_index, cursor, expected_cursor) in [
        ("remove below cursor", 4, 5, 4), ("remove at cursor", 4, 4, 4),
        ("remove after cursor", 5, 2, 2), ("wrap cursor at new count", 5, 5, 0),
    ] {
        let mut host = Host::default();
        let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
        put(&mut io.memory, TASK_COUNT, 4);
        let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
        let sem = call(&mut run, &mut io, 0, "sem_create", &[2, 0]);
        let a = create_task(&mut run, &mut io, 0, 3);
        let b = create_task(&mut run, &mut io, 0, 2);
        for task in [a, b] {
            put(&mut io.memory, CURRENT, task);
            assert_eq!(run.call("sem_take", &[sem, -1], &mut io), 0);
        }
        let removed = if removed_index == 4 { a } else { b };
        let survivor = if removed == a { b } else { a };
        put(&mut io.memory, CURRENT + CURSOR_OFFSET, cursor);
        assert_eq!(call(&mut run, &mut io, 0, "task_delete", &[removed as i32]), 1, "{scenario}");
        assert_eq!(get(&io.memory, TASK_COUNT), 5, "{scenario}: count");
        assert_eq!(get(&io.memory, TASKS + 4 * 4), survivor, "{scenario}: ordered shift");
        assert_eq!(get(&io.memory, TASKS + 5 * 4), 0, "{scenario}: old tail cleared");
        assert_eq!(get(&io.memory, CURRENT + CURSOR_OFFSET), expected_cursor, "{scenario}: cursor");
        assert_eq!(get(&io.memory, sem as u32), survivor, "{scenario}: wait list repaired");
        assert_eq!(get(&io.memory, survivor + TCB_WAIT_NEXT), 0);
        assert_eq!(get(&io.memory, survivor + TCB_WAIT_PREV), sem as u32, "{scenario}: survivor links back to semaphore head");
        assert_eq!(call(&mut run, &mut io, 0, "task_delete", &[survivor as i32]), 1);
        assert_eq!(get(&io.memory, sem as u32), 0, "last waiter removed");
        assert_eq!(call(&mut run, &mut io, 0, "sem_give", &[sem]), 1);
        assert!(io.raised.is_empty(), "no freed waiter is woken");
        assert_eq!(call(&mut run, &mut io, 0, "sem_count", &[sem]), 1);
        call(&mut run, &mut io, 0, "sem_delete", &[sem]);
        assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial, "{scenario}: bytes returned");
        check_heap(&io, 0, 0);
    }
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    put(&mut io.memory, TASK_COUNT, 4);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    let a = create_task(&mut run, &mut io, 0, 2);
    let b = create_task(&mut run, &mut io, 0, 2);
    for (task, handle) in [(a, 0), (b, b as i32)] {
        put(&mut io.memory, CURRENT, task);
        assert_eq!(run.call("task_current", &[], &mut io) as u32, task);
        assert_eq!(run.call("task_delete", &[handle], &mut io), 1);
        assert_eq!(get(&io.memory, task + TCB_STATE), BLOCKED);
        assert_eq!(get64(&io.memory, task + TCB_DEADLINE), u64::MAX);
        assert_eq!(get(&io.memory, get(&io.memory, task + TCB_STACK_BASE)), GUARD, "self-delete retains its stack");
    }
    assert_eq!(get(&io.memory, TASK_COUNT), TASKS_MAX, "self-delete defers reclamation");
    // The zombie marker must win even if a stale deadline looks due.
    put(&mut io.memory, a + TCB_DEADLINE, 0);
    put(&mut io.memory, a + TCB_DEADLINE + 4, 0);
    io.clock = u64::MAX;
    call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
    assert!(io.raised.is_empty(), "zombies never expire");
    for task in [a, b] { assert_eq!(get(&io.memory, task + TCB_STATE), BLOCKED); }
    assert_eq!(call(&mut run, &mut io, 0, "task_reap", &[]), 1, "first adjacent zombie removed");
    assert_eq!(get(&io.memory, TASK_COUNT), 5, "at most one removal per call");
    assert_eq!(get(&io.memory, TASKS + 4 * 4), b, "second zombie remains");
    assert_eq!(call(&mut run, &mut io, 0, "task_reap", &[]), 1, "second adjacent zombie removed");
    assert_eq!(call(&mut run, &mut io, 0, "task_reap", &[]), 0, "no double free");
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    let old = create_task(&mut run, &mut io, 0, 2);
    put(&mut io.memory, CURRENT, old);
    assert_eq!(run.call("task_delete", &[0], &mut io), 1);
    let fresh = create_task(&mut run, &mut io, 0, 2);
    assert_eq!(get(&io.memory, TASK_COUNT), 5, "create reaps before appending");
    assert_eq!(call(&mut run, &mut io, 0, "task_reap", &[]), 0);
    io.clock = 0;
    put(&mut io.memory, CURRENT, fresh);
    assert_eq!(run.call("task_delay", &[10], &mut io), 1);
    assert_eq!(call(&mut run, &mut io, 0, "task_delete", &[fresh as i32]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    check_heap(&io, 0, 0);
}

#[test]
fn task_delay_expiry_table() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    assert_eq!(call(&mut run, &mut io, 1, "task_delay", &[0]), 1);
    assert_eq!(io.raised, [YIELD_LINE]);
    assert_eq!(get(&io.memory, tcb(1) + TCB_STATE), READY);
    let start = 0x7_ffff_fff8;
    io.clock = start;
    for (id, us) in [(1, 10), (2, 20), (3, -1)] {
        assert_eq!(call(&mut run, &mut io, id, "task_delay", &[us]), 1);
        assert_eq!(get(&io.memory, tcb(id) + TCB_STATE), BLOCKED);
        assert_eq!(get64(&io.memory, tcb(id) + TCB_DEADLINE),
            start + u64::from(us as u32) * u64::from(TICKS_PER_US), "all nonzero delays are finite");
        assert!(io.raised.is_empty());
    }
    put(&mut io.memory, tcb(4) + TCB_STATE, BLOCKED); // R3 dispatcher wait, no object
    for (us, next, wakes) in [(9, 10, vec![]), (10, 20, vec![1]), (20, u64::from(u32::MAX), vec![2])] {
        io.clock = start + us * u64::from(TICKS_PER_US);
        call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
        assert_eq!(io.raised, vec![YIELD_LINE; wakes.len()]);
        for id in wakes { completed(&io, id, 1, "sleep completion"); }
        assert_eq!(get64(&io.memory, DEADLINE_OUT), start + next * u64::from(TICKS_PER_US));
        assert_eq!(get(&io.memory, tcb(4) + TCB_STATE), BLOCKED, "dispatcher untouched");
    }
    io.clock = u64::MAX;
    call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
    completed(&io, 3, 1, "maximum unsigned delay expires");
    assert_eq!(get64(&io.memory, DEADLINE_OUT), u64::MAX);
}

#[test]
fn osi_ticks_and_blocking_results_table() {
    let mut run = Run::new(compiled_objects());
    for name in ["osi_semphr_take", "osi_queue_recv", "osi_queue_send_to_back",
        "osi_queue_send_to_front", "osi_task_delay"] {
        for ticks in [0u32, 1, 3, 429_496, 429_497, 0xffff_fffe, u32::MAX] {
            let mut host = Host::default();
            let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
            let object = if name == "osi_semphr_take" {
                call(&mut run, &mut io, 0, "sem_create", &[1, 0])
            } else { call(&mut run, &mut io, 0, "queue_create", &[1, 4]) };
            assert_ne!(object, 0);
            if name.starts_with("osi_queue_send") {
                assert_eq!(call(&mut run, &mut io, 0, "queue_send_back", &[object, buffer(0), 0]), 1);
            }
            let args = if name == "osi_task_delay" { vec![ticks as i32] }
                else if name == "osi_semphr_take" { vec![object, ticks as i32] }
                else { vec![object, buffer(1), ticks as i32] };
            let start = 0x7_ffff_fff8;
            io.clock = start;
            assert_eq!(call(&mut run, &mut io, 1, name, &args), 0, "{name}/{ticks}: no immediate success");
            assert_eq!(get(&io.memory, tcb(1) + TCB_STATE), if ticks == 0 { READY } else { BLOCKED }, "{name}/{ticks}");
            if ticks == 0 { continue; }
            let deadline = if ticks == u32::MAX { u64::MAX } else {
                start + (u64::from(ticks) * 10_000).min(u64::from(u32::MAX - 1)) * u64::from(TICKS_PER_US)
            };
            assert_eq!(get64(&io.memory, tcb(1) + TCB_DEADLINE), deadline, "{name}/{ticks}: tick units and saturation");
            io.clock = deadline - 1;
            call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
            assert_eq!(get(&io.memory, tcb(1) + TCB_STATE), BLOCKED, "{name}/{ticks}: before deadline");
            io.clock = deadline;
            call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
            if ticks == u32::MAX {
                assert_eq!(get(&io.memory, tcb(1) + TCB_STATE), BLOCKED, "{name}: forever never expires");
            } else {
                completed(&io, 1, u32::from(name == "osi_task_delay"), name);
            }
        }
    }
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    for ms in [0u32, 1, 9, 10, 11, 999, 1000, 0x7fff_ffff, 0x8000_0000, u32::MAX] {
        assert_eq!(call(&mut run, &mut io, 0, "osi_task_ms_to_tick", &[ms as i32]) as u32,
            ms / 10, "IDF's unsigned floor for {ms} milliseconds");
    }
    let spin = call(&mut run, &mut io, 0, "osi_spin_lock_create", &[]);
    assert_ne!(spin, 0);
    assert_eq!(call(&mut run, &mut io, 0, "osi_semphr_take", &[spin, 0]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "osi_semphr_take", &[spin, 0]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "sem_give", &[spin]), 1);
    call(&mut run, &mut io, 0, "sem_delete", &[spin]);
    for (create, recursive) in [("osi_mutex_create", false), ("osi_recursive_mutex_create", true)] {
        let mutex = call(&mut run, &mut io, 0, create, &[]);
        assert_ne!(mutex, 0);
        assert_eq!(call(&mut run, &mut io, 0, "osi_mutex_lock", &[mutex]), 1);
        if recursive {
            assert_eq!(call(&mut run, &mut io, 0, "osi_mutex_lock", &[mutex]), 1);
            assert_eq!(call(&mut run, &mut io, 0, "mutex_unlock", &[mutex]), 1);
        }
        assert_eq!(call(&mut run, &mut io, 1, "osi_mutex_lock", &[mutex]), 0);
        assert_eq!(get64(&io.memory, tcb(1) + TCB_DEADLINE), u64::MAX);
        assert_eq!(call(&mut run, &mut io, 0, "mutex_unlock", &[mutex]), 1);
        completed(&io, 1, 1, "OSI mutex wake returns pdTRUE");
        assert_eq!(call(&mut run, &mut io, 1, "mutex_unlock", &[mutex]), 1);
        call(&mut run, &mut io, 0, "mutex_delete", &[mutex]);
    }
}

#[test]
fn osi_tasks_thread_semaphores_and_reap_one() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    put(&mut io.memory, TASK_COUNT, 2);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    let mut tasks = Vec::new();
    let mut sems = Vec::new();
    for p in [0, 24] {
        let stack_bytes = if p == 0 { 512 } else { 0 };
        assert_eq!(call(&mut run, &mut io, 0, "osi_task_create", &[0x4000_2000, 0, stack_bytes, 123, p, BUFFERS as i32]), 1);
        let task = get(&io.memory, BUFFERS);
        assert_eq!(task & 7, 0);
        let stack = get(&io.memory, task + TCB_STACK_BASE);
        let top = get(&io.memory, task + TCB_STACK_TOP);
        assert!(top - stack >= (stack_bytes as u32).max(MIN_STACK_BYTES), "Xtensa stack depth is bytes");
        assert_eq!(get(&io.memory, task + TCB_PRIORITY), p as u32 + 1);
        assert_eq!(get(&io.memory, task + TCB_THREAD_SEM), 0);
        put(&mut io.memory, CURRENT, task);
        let sem = run.call("osi_wifi_thread_semphr_get", &[], &mut io);
        assert_ne!(sem, 0);
        assert_eq!(get(&io.memory, task + TCB_THREAD_SEM), sem as u32);
        assert_eq!(run.call("sem_count", &[sem], &mut io), 0, "thread sem starts empty");
        let allocated = run.call("heap_free_bytes", &[], &mut io);
        assert_eq!(run.call("osi_wifi_thread_semphr_get", &[], &mut io), sem);
        assert_eq!(run.call("heap_free_bytes", &[], &mut io), allocated, "only one semaphore per task");
        tasks.push(task);
        sems.push(sem);
    }
    assert_ne!(sems[0], sems[1], "thread semaphores are per task");
    // FreeRTOS clamps a priority at or above configMAX_PRIORITIES to the top one.
    assert_eq!(call(&mut run, &mut io, 0, "osi_task_create", &[0x4000_2000, 0, 0, 0, 25, BUFFERS as i32]), 1);
    let clamped = get(&io.memory, BUFFERS);
    assert_eq!(get(&io.memory, clamped + TCB_PRIORITY), BLOB_MAX_PRIORITY as u32, "pack priority 25 clamps to 24, kernel 25");
    call(&mut run, &mut io, 0, "osi_task_delete", &[clamped as i32]);
    call(&mut run, &mut io, 0, "osi_task_delete", &[tasks[1] as i32]);
    assert_eq!(get(&io.memory, TASK_COUNT), 3, "non-current deletion frees the TCB, stack and thread sem");
    put(&mut io.memory, CURRENT, tasks[0]);
    run.call("osi_task_delete", &[0], &mut io);
    assert_eq!(get(&io.memory, tasks[0] + TCB_STATE), BLOCKED, "delete(NULL) is self-delete");
    assert_eq!(get(&io.memory, tasks[0] + TCB_THREAD_SEM), 0, "self-delete releases its semaphore immediately");
    assert_eq!(run.call("task_reap", &[], &mut io), 0, "cannot reclaim the current stack");
    assert_eq!(call(&mut run, &mut io, 0, "task_reap", &[]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial, "all thread semaphore allocations returned");
    check_heap(&io, 0, 0);

    // Two adjacent zombies: task_create reaps just one, then task_reap drains
    // the second. Each takes its thread semaphore first; self-delete frees it,
    // so a zombie never holds one and reaping frees only the TCB and stack.
    tasks.clear();
    for _ in 0..2 {
        assert_eq!(call(&mut run, &mut io, 0, "osi_task_create", &[0x4000_2000, 0, 0, 0, 0, BUFFERS as i32]), 1);
        tasks.push(get(&io.memory, BUFFERS));
    }
    for &task in &tasks {
        put(&mut io.memory, CURRENT, task);
        assert_ne!(run.call("osi_wifi_thread_semphr_get", &[], &mut io), 0, "the task takes its thread semaphore before deleting itself");
        run.call("osi_task_delete", &[0], &mut io);
        assert_eq!(get(&io.memory, task + TCB_THREAD_SEM), 0, "a zombie holds no thread semaphore (objects.splash remove_task invariant)");
    }
    assert_eq!(call(&mut run, &mut io, 0, "osi_task_create", &[0x4000_2000, 0, 0, 0, 0, BUFFERS as i32]), 1);
    let fresh = get(&io.memory, BUFFERS);
    assert_eq!(get(&io.memory, TASK_COUNT), 4, "create calls reap once, then appends");
    assert_eq!(get(&io.memory, TASKS + 8), tasks[1], "one zombie remains");
    assert_eq!(call(&mut run, &mut io, 0, "task_reap", &[]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "task_reap", &[]), 0);
    call(&mut run, &mut io, 0, "osi_task_delete", &[fresh as i32]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial, "self-delete freed each thread sem and reap freed each TCB and stack, with no leak or double free");
    check_heap(&io, 0, 0);
}

#[test]
fn osi_wifi_queue_and_word_sized_isr_wake() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    assert_eq!(call(&mut run, &mut io, 0, "osi_wifi_create_queue", &[0, 4]), 0, "failed queue releases its wrapper");
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    let wrapper = call(&mut run, &mut io, 0, "osi_wifi_create_queue", &[2, 4]);
    assert_ne!(wrapper, 0);
    let q = get(&io.memory, wrapper as u32) as i32;
    assert_ne!(q, 0);
    assert_ne!(q, wrapper);
    assert_eq!(get(&io.memory, wrapper as u32 + 4), 0, "queue owns its storage");
    let input = BUFFERS as i32;
    let output = (BUFFERS + 16) as i32;
    for (name, value) in [("osi_queue_send_to_back", 10), ("osi_queue_send_to_front", 20)] {
        put(&mut io.memory, input as u32, value);
        assert_eq!(call(&mut run, &mut io, 0, name, &[q, input, 0]), 1);
    }
    for value in [20, 10] {
        assert_eq!(call(&mut run, &mut io, 0, "osi_queue_recv", &[q, output, 0]), 1);
        assert_eq!(get(&io.memory, output as u32), value, "front/back ordering");
    }
    const WORD_WOKEN: u32 = BUFFERS + 32;
    for higher in [false, true] {
        put(&mut io.memory, tcb(1) + TCB_PRIORITY, if higher { 25 } else { 1 });
        assert_eq!(call(&mut run, &mut io, 1, "osi_queue_recv", &[q, output, -1]), 0);
        put(&mut io.memory, WORD_WOKEN, 0xa5a5_a500);
        put(&mut io.memory, WORD_WOKEN + 4, 0x1234_5678);
        put(&mut io.memory, input as u32, 42);
        assert_eq!(call(&mut run, &mut io, 0, "osi_queue_send_from_isr", &[q, input, WORD_WOKEN as i32]), 1);
        assert_eq!(get(&io.memory, WORD_WOKEN), if higher { 1 } else { 0xa5a5_a500 }, "IDF BaseType_t wake flag");
        assert_eq!(get(&io.memory, WORD_WOKEN + 4), 0x1234_5678, "adjacent word untouched");
        assert!(io.raised.is_empty(), "ISR wrapper leaves selection to the scheduler");
        completed(&io, 1, 1, "ISR queue wake");
        assert_eq!(get(&io.memory, output as u32), 42);
    }
    call(&mut run, &mut io, 0, "osi_wifi_delete_queue", &[wrapper]);
    call(&mut run, &mut io, 0, "osi_wifi_delete_queue", &[0]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial, "queue and wrapper freed");
    check_heap(&io, 0, 0);
}

#[test]
fn osi_time_words_seconds_and_milliseconds_table() {
    let mut run = Run::new(compiled_objects());
    let high = run.module.vars().iter().find(|v| run.module.string(v.name) == "osi_time_high").unwrap().offset as usize;
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    let out = BUFFERS + 4;
    for us in [0u64, 999, 1000, 999_999, 1_000_000, 1_234_567, 0xffff_ffff, 0x1_0000_0000,
        0x1_0000_02c0, (1u64 << 32) * 1000 - 1, (1u64 << 32) * 1000,
        (1u64 << 32) * 1000 + 1000, 0x12_3456_789a, ((1u64 << 52) - 1) / u64::from(TICKS_PER_US)] {
        io.clock = us * u64::from(TICKS_PER_US) + u64::from(TICKS_PER_US - 1);
        let low = call(&mut run, &mut io, 0, "osi_esp_timer_get_time", &[]) as u32;
        assert_eq!(u64::from(low) | (u64::from(run.state[high]) << 32), us);
        assert_eq!(call(&mut run, &mut io, 0, "osi_log_timestamp", &[]) as u32, (us / 1000) as u32,
            "{us}: full microsecond value divided before wrapping milliseconds");
        put(&mut io.memory, out - 4, 0x1234_5678);
        put(&mut io.memory, out + 8, 0x8765_4321);
        assert_eq!(call(&mut run, &mut io, 0, "osi_get_time", &[out as i32]), 0);
        assert_eq!(get(&io.memory, out), (us / 1_000_000) as u32, "{us}: seconds");
        assert_eq!(get(&io.memory, out + 4), (us % 1_000_000) as u32, "{us}: microseconds");
        assert_eq!(get(&io.memory, out - 4), 0x1234_5678);
        assert_eq!(get(&io.memory, out + 8), 0x8765_4321);
    }
}

// Keep the reference's access checks and IRQ rules, but let device reads vary
// between port calls and record successful reset/IRQ writes. No rt changes needed.
struct DevicePorts<'a> {
    io: KernelPorts<'a>,
    clocks_us: VecDeque<u64>,
    rng_words: VecDeque<u32>,
    clock_reads: usize,
    rng_reads: usize,
    reset_writes: Vec<u32>,
    irq_writes: Vec<(u32, u32)>,
}
impl<'a> DevicePorts<'a> {
    fn new(host: &'a mut Host) -> Self {
        Self { io: KernelPorts::new(memory(), RADIO_KERNEL_LINES, host),
            clocks_us: VecDeque::new(), rng_words: VecDeque::new(), clock_reads: 0,
            rng_reads: 0, reset_writes: Vec::new(), irq_writes: Vec::new() }
    }
}
impl Ports for DevicePorts<'_> {
    fn call(&mut self, port: u16, site: u32, ops: u64, args: &[u32], rets: &mut [u32]) -> PortCall {
        if port == rt::ports::NOW64 {
            self.clock_reads += 1;
            if let Some(us) = self.clocks_us.pop_front() { self.io.clock = us * u64::from(TICKS_PER_US); }
        }
        let result = self.io.call(port, site, ops, args, rets);
        if result == PortCall::Done {
            if port == rt::ports::DEV_LOAD32 && args[0] == RNG_DATA {
                self.rng_reads += 1;
                rets[0] = self.rng_words.pop_front().expect("unexpected RNG read");
            } else if port == rt::ports::DEV_STORE32 && args[0] == SYSCON_WIFI_RST_EN {
                self.reset_writes.push(args[1]);
            } else if (port == rt::ports::DEV_STORE32 && (INTR_MATRIX..INTR_MATRIX + WIFI_SOURCES * 4).contains(&args[0]))
                || (port == rt::ports::STORE32 && (ISRS..ISR_COUNT + 4).contains(&args[0])) {
                self.irq_writes.push((args[0], args[1]));
            }
        }
        result
    }
}

#[test]
fn osi_interrupt_routing_registration_and_reports() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut devices = DevicePorts::new(&mut host);
    devices.io.clock = 1234;
    let mut expected_reports = Vec::new();
    for source in 0..WIFI_SOURCES { put(&mut devices.io.memory, INTR_MATRIX + source * 4, 16 + source); }
    for source in [0, 2] {
        assert_eq!(run.call("osi_set_intr", &[99, source, 0, -1], &mut devices), 0,
            "CPU and priority are ignored; source {source} routes to line 0");
    }
    assert_eq!(devices.irq_writes, [(INTR_MATRIX, 0), (INTR_MATRIX + 8, 0)]);
    for (source, expected) in [0, 17, 0, 19].into_iter().enumerate() {
        assert_eq!(get(&devices.io.memory, INTR_MATRIX + source as u32 * 4), expected);
    }
    let matrix = devices.io.memory.bytes(INTR_MATRIX, (WIFI_SOURCES * 4) as usize).unwrap().to_vec();
    let refusals = [(4, 0, FAULT_KERNEL_IRQ_SOURCE_REFUSED, 4),
        (-1, 32, FAULT_KERNEL_IRQ_SOURCE_REFUSED, -1)].into_iter()
        .chain([1, 2, 6, 7, 11, 32, -1].into_iter().map(|n| (0, n, FAULT_KERNEL_IRQ_LINE_REFUSED, n)));
    for (source, line, code, arg) in refusals {
        devices.irq_writes.clear();
        assert_eq!(run.call("osi_set_intr", &[0, source, line, 1], &mut devices), 0);
        assert!(devices.irq_writes.is_empty(), "source {source}, line {line}: no matrix store");
        assert_eq!(devices.io.memory.bytes(INTR_MATRIX, matrix.len()).unwrap(), matrix);
        expected_reports.push((1234, code, arg));
    }

    assert_eq!(PACK_ISR_LINES, 1, "the pack registers only line 0");
    let entry = SHIM + 0x100;
    let replacement = SHIM + 0x200;
    devices.irq_writes.clear();
    assert_eq!(run.call("osi_set_isr", &[3, 0, -1], &mut devices), 0);
    assert!(devices.irq_writes.is_empty(), "NULL for an absent line does nothing");
    assert_eq!(run.call("osi_set_isr", &[0, entry as i32, 0x1234], &mut devices), 0);
    assert_eq!(get(&devices.io.memory, ISR_COUNT), 1);
    assert_eq!((get(&devices.io.memory, ISRS), get(&devices.io.memory, ISRS + 4), get(&devices.io.memory, ISRS + 8)),
        (1, entry, 0x1234));
    devices.irq_writes.clear();
    assert_eq!(run.call("osi_set_isr", &[0, replacement as i32, -7], &mut devices), 0);
    assert_eq!(devices.irq_writes, [(ISRS + 4, replacement), (ISRS + 8, (-7i32) as u32)], "replace in place");
    assert_eq!(get(&devices.io.memory, ISR_COUNT), 1, "replacement consumes no slot");
    let table = devices.io.memory.bytes(ISRS, (ISR_COUNT - ISRS + 4) as usize).unwrap().to_vec();
    let wrong_region = 0x8000_0200u32 as i32;
    for (line, f, code, arg) in [(1, entry as i32, FAULT_KERNEL_IRQ_LINE_REFUSED, 1),
        (32, entry as i32, FAULT_KERNEL_IRQ_LINE_REFUSED, 32),
        (0, wrong_region, FAULT_KERNEL_ISR_ENTRY_REFUSED, wrong_region),
        (3, entry as i32, FAULT_KERNEL_ISR_TABLE_FULL, 3)] {
        devices.irq_writes.clear();
        assert_eq!(run.call("osi_set_isr", &[line, f, 17], &mut devices), 0);
        assert!(devices.irq_writes.is_empty(), "refused line {line}, entry {f:#x}: no table store");
        assert_eq!(devices.io.memory.bytes(ISRS, table.len()).unwrap(), table);
        expected_reports.push((1234, code, arg));
    }
    assert_eq!(run.call("osi_set_isr", &[3, 0, 17], &mut devices), 0);
    assert!(devices.irq_writes.is_empty(), "absent NULL also succeeds when full");
    assert_eq!(run.call("osi_set_isr", &[0, 0, 17], &mut devices), 0);
    assert_eq!(get(&devices.io.memory, ISR_COUNT), 0, "NULL removes the live slot");
    devices.irq_writes.clear();
    assert_eq!(run.call("osi_set_isr", &[0, 0, 17], &mut devices), 0);
    assert!(devices.irq_writes.is_empty(), "removing twice does nothing");
    assert!(devices.io.fault.is_none(), "reports do not suspend the export");
    drop(devices);
    assert_eq!(host.reports, expected_reports, "Hal::fault carries each port-78 code, arg and clock");
}

#[test]
fn osi_isr_removal_moves_the_last_live_slot() {
    // Exercise the generic R3 table operation with three slots, while the
    // production-layout test above uses exactly PACK_ISR_LINES.
    let layout = KernelLayout { isr_capacity: 3, ..layout() };
    let compiled = compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES, &kernel_consts(&layout)).unwrap();
    let mut run = Run::new(&compiled);
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    for line in [0, 3, 10] {
        assert_eq!(run.call("osi_set_isr", &[line, (SHIM + 0x100) as i32, line + 100], &mut io), 0);
    }
    assert_eq!(get(&io.memory, ISR_COUNT), 3);
    assert_eq!(run.call("osi_set_isr", &[10, (SHIM + 0x200) as i32, 999], &mut io), 0);
    for (line, count, slot) in [(3, 2, ISRS + ISR_SLOT), (0, 1, ISRS)] {
        assert_eq!(run.call("osi_set_isr", &[line, 0, 0], &mut io), 0);
        assert_eq!(get(&io.memory, ISR_COUNT), count);
        assert_eq!((get(&io.memory, slot), get(&io.memory, slot + 4), get(&io.memory, slot + 8)),
            (1 << 10, SHIM + 0x200, 999), "the last mask, entry and arg move together");
    }
    assert_eq!(run.call("osi_set_isr", &[10, 0, 0], &mut io), 0);
    assert_eq!(get(&io.memory, ISR_COUNT), 0);
}

#[test]
fn osi_device_mac_reset_calibration_interrupts_and_prints() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host::default();
    let mut devices = DevicePorts::new(&mut host);
    assert_eq!(run.call("print_dropped", &[], &mut devices), 0);
    for (i, name) in ["puts", "net80211_printf", "pp_printf", "phy_printf"].iter().enumerate() {
        assert_eq!(run.call(name, &[0], &mut devices), 0, "{name} drops the line");
        assert_eq!(run.call("print_dropped", &[], &mut devices), (i + 1) as i32, "{name} counts once");
    }
    for address in [EFUSE_MAC_0, EFUSE_MAC_1, RTC_STORE1, RNG_DATA] {
        assert!(devices.io.memory.bytes_mut(address, 4).is_err(), "{address:#x}: read-only device");
    }
    for kind in 0..=2 {
        devices.io.memory.bytes_mut(WOKEN - 1, 8).unwrap().fill(0xa5);
        assert_eq!(run.call("osi_read_mac", &[WOKEN as i32, kind], &mut devices), 0, "MAC type {kind}");
        assert_eq!(devices.io.memory.bytes(WOKEN - 1, 8).unwrap(),
            &[0xa5, 0x02, 0x34, 0x89, 0xab, 0xcd, 0xffu8.wrapping_add(kind as u8), 0xa5],
            "type {kind}: high byte first, ignore eFuse upper bits, wrap only the last byte");
    }
    for kind in [3, 4, -1] {
        devices.io.memory.bytes_mut(WOKEN - 1, 8).unwrap().fill(0x5a);
        assert_eq!(run.call("osi_read_mac", &[WOKEN as i32, kind], &mut devices), -1);
        assert_eq!(devices.io.memory.bytes(WOKEN - 1, 8).unwrap(), &[0x5a; 8], "invalid type writes nothing");
        assert_eq!(run.call("osi_read_mac", &[0, kind], &mut devices), -1, "invalid type never touches output");
    }
    assert_eq!(get(&devices.io.memory, EFUSE_MAC_0), 0x89ab_cdff);
    assert_eq!(get(&devices.io.memory, EFUSE_MAC_1), 0xbeef_0234);
    assert_eq!(run.call("osi_slowclk_cal_get", &[], &mut devices) as u32, 0x01fd_b975,
        "19 to 12 fractional bits, with an unsigned shift");
    for before in [0xa5a5_5a5au32, 0xa5a5_5a5e] {
        put(&mut devices.io.memory, SYSCON_WIFI_RST_EN, before);
        devices.reset_writes.clear();
        run.call("osi_wifi_reset_mac", &[], &mut devices);
        assert_eq!(devices.reset_writes, [before | WIFIMAC_RST, before & !WIFIMAC_RST], "set then clear reset");
        assert_eq!(get(&devices.io.memory, SYSCON_WIFI_RST_EN), before & !WIFIMAC_RST, "preserve every other bit");
    }
    let protected = RADIO_KERNEL_LINES.protected;
    for before in [0, u32::MAX, 0xaaaa_aaaa, 0x5555_5555] {
        for mask in [0, protected, 1 << 5, u32::MAX] {
            devices.io.intenable = before;
            run.call("osi_ints_on", &[mask as i32], &mut devices);
            assert_eq!(devices.io.intenable, before | (mask & !protected), "enable {mask:#x} from {before:#x}");
            devices.io.intenable = before;
            run.call("osi_ints_off", &[mask as i32], &mut devices);
            assert_eq!(devices.io.intenable, before & !(mask & !protected), "disable {mask:#x} from {before:#x}");
        }
    }
}

#[test]
fn osi_random_spacing_xor_wrap_and_stalled_clock() {
    let mut run = Run::new(compiled_objects());
    let last = run.module.vars().iter().find(|v| run.module.string(v.name) == "osi_rng_last").unwrap().offset as usize;
    let mut host = Host::default();
    let mut devices = DevicePorts::new(&mut host);
    assert_eq!(RNG_SPACING_US, 2);
    assert!(RNG_POLL_CAP >= 64);
    assert_eq!(run.state[last], 0, "RNG timestamp starts at boot");
    for (us, word) in [(10u64, 0x89ab_cdefu32), (13, 0x7654_3210)] {
        devices.clocks_us.push_back(us);
        devices.rng_words.push_back(word);
        assert_eq!(run.call("osi_random", &[], &mut devices) as u32, word, "spaced call folds one final read");
        assert_eq!(run.state[last], us as u32);
        assert!(devices.clocks_us.is_empty() && devices.rng_words.is_empty());
    }
    devices.clocks_us.extend([13, 14, 15]);
    devices.rng_words.extend([0xf0f0_0f0f, 0x1234_5678, 0x89ab_cdef]);
    assert_eq!(run.call("osi_random", &[], &mut devices) as u32, 0xf0f0_0f0f ^ 0x1234_5678 ^ 0x89ab_cdef,
        "fold both early polls and the final word at exactly two microseconds");
    assert_eq!(run.state[last], 15);
    assert!(devices.clocks_us.is_empty() && devices.rng_words.is_empty());

    devices.clocks_us.push_back(0xffff_ffff);
    devices.rng_words.push_back(0x1357_9bdf);
    assert_eq!(run.call("osi_random", &[], &mut devices) as u32, 0x1357_9bdf, "unsigned elapsed comparison");
    devices.clocks_us.extend([0x1_0000_0000, 0x1_0000_0001]);
    devices.rng_words.extend([0xaaaa_5555, 0x2468_ace0]);
    assert_eq!(run.call("osi_random", &[], &mut devices) as u32, 0xaaaa_5555 ^ 0x2468_ace0,
        "spacing survives the low microsecond word wrapping");
    assert_eq!(run.state[last], 1);
    assert!(devices.clocks_us.is_empty() && devices.rng_words.is_empty());

    // No clock updates: every bounded poll sees the previous call's time.
    let words: Vec<_> = (0..=RNG_POLL_CAP).map(|i| 0x9e37_79b9u32.rotate_left(i) ^ i).collect();
    let expected = words.iter().fold(0, |r, &word| r ^ word);
    devices.rng_words.extend(words);
    let clocks_before = devices.clock_reads;
    let reads_before = devices.rng_reads;
    assert_eq!(run.call("osi_random", &[], &mut devices) as u32, expected, "cap returns the accumulated XOR");
    assert_eq!(devices.clock_reads - clocks_before, RNG_POLL_CAP as usize, "exactly the capped polls");
    assert_eq!(devices.rng_reads - reads_before, RNG_POLL_CAP as usize + 1, "capped folds plus final read");
    assert_eq!(run.state[last], 1, "last observed microsecond word retained");
    assert!(devices.rng_words.is_empty());
    assert!(devices.io.fault.is_none(), "cap proceeds without a fault");
}

const ETS_BASE: u32 = 0x1_0000;
const ETS_BYTES: usize = 20;
const ETS_TAG: u32 = 0x1212_1212;
const ETS_PENDING_TAG: u32 = 0x1212_1213;
fn timer_words(io: &KernelPorts<'_>, key: i32) -> [u32; 5] {
    [0, 4, 8, 12, 16].map(|offset| get(&io.memory, key as u32 + offset))
}
fn timer_key(index: usize) -> i32 { (ETS_BASE + (index * ETS_BYTES) as u32) as i32 }
fn with_timer_storage(mut memory: KernelMemory, keys: usize) -> KernelMemory {
    memory.add_region(ETS_BASE, vec![0; keys * ETS_BYTES], true, RegionKind::Pack).unwrap();
    memory
}

fn due(run: &mut Run<'_>, io: &mut KernelPorts<'_>, expected: Option<(u32, u32)>, next: u64, scenario: &str) {
    io.memory.bytes_mut(DEADLINE_OUT - 4, 24).unwrap().fill(0xa5);
    assert_eq!(call(run, io, 0, "timer_due", &[DEADLINE_OUT as i32]), i32::from(expected.is_some()), "{scenario}: due");
    let (func, arg) = expected.unwrap_or((0xa5a5_a5a5, 0xa5a5_a5a5));
    assert_eq!(get(&io.memory, DEADLINE_OUT), func, "{scenario}: callback or untouched word");
    assert_eq!(get(&io.memory, DEADLINE_OUT + 4), arg, "{scenario}: argument or untouched word");
    assert_eq!(get64(&io.memory, DEADLINE_OUT + 8), next, "{scenario}: next armed deadline");
    assert_eq!(get(&io.memory, DEADLINE_OUT - 4), 0xa5a5_a5a5, "{scenario}: lower output guard");
    assert_eq!(get(&io.memory, DEADLINE_OUT + 16), 0xa5a5_a5a5, "{scenario}: upper output guard");
}

#[test]
fn timer_order_reinitialization_period_and_saturation_table() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(with_timer_storage(memory(), 3), RADIO_KERNEL_LINES, &mut host);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    let keys = [0, 1, 2].map(timer_key);
    for name in ["timer_disarm", "timer_done", "timer_arm", "timer_arm_us"] {
        let args = if name.starts_with("timer_arm") { vec![keys[0], 1, 0] } else { vec![keys[0]] };
        assert_eq!(call(&mut run, &mut io, 0, name, &args), 0, "unknown key: {name}");
    }
    due(&mut run, &mut io, None, u64::MAX, "initially empty");
    for (i, &key) in keys.iter().enumerate() {
        assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[key, 100 + i as i32, 200 + i as i32]), 1);
        if i == 0 { assert_eq!(call(&mut run, &mut io, 0, "timer_slots", &[]), 64, "first setfn reserves a chunk"); }
    }
    let start = 0x7_ffff_fff0;
    let tick = u64::from(TICKS_PER_US);
    io.clock = start;
    for (key, us) in [(keys[0], 30), (keys[1], 10), (keys[2], 10), (keys[1], 10)] {
        assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[key, us, 0]), 1);
    }
    due(&mut run, &mut io, None, start + 10 * tick, "earliest deadline with carry");
    io.clock = start + 10 * tick - 1;
    due(&mut run, &mut io, None, start + 10 * tick, "one tick early");
    io.clock += 1;
    due(&mut run, &mut io, Some((102, 202)), start + 10 * tick, "rearm moves behind FIFO tie");
    due(&mut run, &mut io, Some((101, 201)), start + 30 * tick, "second equal deadline");
    assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[keys[0], 5, 1]), 1);
    due(&mut run, &mut io, None, start + 15 * tick, "rearm moves earlier");
    let before = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[keys[0], 111, 211]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), before, "second setfn keeps its node");
    due(&mut run, &mut io, None, start + 15 * tick, "second setfn preserves the armed deadline");
    assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[keys[0], 0, 1]), 1);
    due(&mut run, &mut io, Some((100, 200)), u64::MAX, "zero period is one shot");
    due(&mut run, &mut io, None, u64::MAX, "one shot fires exactly once");
    assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[keys[0], 10, 7]), 1);
    io.clock = start + 20 * tick;
    due(&mut run, &mut io, Some((100, 200)), start + 30 * tick, "periodic rearm from deadline");
    io.clock = start + 65 * tick;
    due(&mut run, &mut io, Some((100, 200)), start + 75 * tick, "late timer rearms from now");
    due(&mut run, &mut io, None, start + 75 * tick, "late timer does not burst");
    for _ in 0..2 { assert_eq!(call(&mut run, &mut io, 0, "timer_disarm", &[keys[0]]), 1); }
    due(&mut run, &mut io, None, u64::MAX, "disarm is idempotent for a known key");
    for (ms, us) in [(0, 0), (1, 1000), (4_294_967, 4_294_967_000u64),
        (4_294_968, u64::from(u32::MAX)), (-1, u64::from(u32::MAX))] {
        io.clock = start;
        assert_eq!(call(&mut run, &mut io, 0, "timer_arm", &[keys[0], ms, 0]), 1);
        let deadline = start + us * tick;
        if us != 0 { due(&mut run, &mut io, None, deadline, "millisecond saturation"); }
        io.clock = deadline;
        due(&mut run, &mut io, Some((100, 200)), u64::MAX, "saturated one shot fires at exact deadline");
    }
    // Remove an armed middle node and both ends without disturbing survivors.
    io.clock = start;
    for &key in &keys { assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[key, 5, 0]), 1); }
    assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[keys[1]]), 1);
    io.clock += 5 * tick;
    due(&mut run, &mut io, Some((100, 200)), start + 5 * tick, "done unlinks the middle armed node");
    due(&mut run, &mut io, Some((102, 202)), u64::MAX, "remaining tail");
    for &key in &[keys[0], keys[2]] { assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[key]), 1); }
    assert_eq!(call(&mut run, &mut io, 0, "timer_slots", &[]), 0, "last done releases the chunk");
    assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[keys[0]]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial, "all timer bytes returned");
    let all = call(&mut run, &mut io, 0, "malloc", &[initial]);
    assert_ne!(all, 0);
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[keys[0], 1, 2]), 0, "setfn heap exhaustion");
    assert_eq!(call(&mut run, &mut io, 0, "timer_failures", &[]), 1, "heap exhaustion is visible too");
    let refused = io.clock;
    call(&mut run, &mut io, 0, "free", &[all]);
    due(&mut run, &mut io, None, u64::MAX, "failed setfn leaves no node");
    check_heap(&io, 0, 0);
    drop(io);
    assert_eq!(host.reports, [(0, FAULT_KERNEL_TIMER_UNSET, keys[0]), (0, FAULT_KERNEL_TIMER_UNSET, keys[0]),
        (refused, FAULT_KERNEL_TIMER_REFUSED, 0)]);
}

#[test]
fn timer_heap_refusals_and_slot_reuse() {
    const LIVE: usize = 8;
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(with_timer_storage(memory(), LIVE + 1), RADIO_KERNEL_LINES, &mut host);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    for i in 0..LIVE {
        assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[timer_key(i), 100 + i as i32, 200]), 1);
    }
    let free = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    let held = call(&mut run, &mut io, 0, "malloc", &[free]);
    assert_ne!(held, 0);
    let extra = timer_key(LIVE);
    let first = timer_key(0);
    for failures in 1..=2 {
        io.clock = failures as u64;
        io.memory.bytes_mut(extra as u32, ETS_BYTES).unwrap().fill(0xa5);
        assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[extra, 900, 901]), 0);
        assert_eq!(timer_words(&io, extra), [0, ETS_PENDING_TAG, 0, 900, 901]);
        assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[extra]), 1);
        assert_eq!(timer_words(&io, extra), [0; 5], "done clears pending words");
        assert_eq!(call(&mut run, &mut io, 0, "timer_failures", &[]), failures);
        assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), 0);
    }
    assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[first, 0, 0]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[first, 300, 301]), 1, "known timer needs no allocation");
    due(&mut run, &mut io, Some((100, 200)), u64::MAX, "ignored setfn on a full heap");
    assert_eq!(call(&mut run, &mut io, 0, "heap_failures", &[]), 0, "timer failures only count as refusals");
    assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[first]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[first]), 0, "second done releases nothing");
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[extra, 900, 901]), 1, "freed node space is reusable");
    io.clock = 3;
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[first, 300, 301]), 0, "heap full again");
    for i in 1..=LIVE { assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[timer_key(i)]), 1); }
    call(&mut run, &mut io, 0, "free", &[held]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    assert_eq!(call(&mut run, &mut io, 0, "timer_high_water", &[]), LIVE as i32, "peak survives frees");
    assert_eq!(call(&mut run, &mut io, 0, "timer_failures", &[]), 3, "exact refusal count survives frees");
    check_heap(&io, 0, 0);
    drop(io);
    assert_eq!(host.reports, [1, 2].map(|tick| (tick, FAULT_KERNEL_TIMER_REFUSED, LIVE as i32)));
}

#[test]
fn timer_handle_lifetime_and_invalid_reports() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(with_timer_storage(memory(), 2), RADIO_KERNEL_LINES, &mut host);
    let (ets, copy) = (timer_key(0), timer_key(1));
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    io.memory.bytes_mut(ets as u32, ETS_BYTES).unwrap().fill(0xa5);
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[ets, 11, 22]), 1);
    let handle = get(&io.memory, ets as u32 + 16);
    assert_eq!(handle & 7, 0);
    assert!((HEAP + radio::kernel::heap_control_bytes(HEAP_BYTES) + 16..=HEAP + HEAP_BYTES - 36).contains(&handle));
    assert_eq!([0, 4, 8, 12, 16].map(|offset| get(&io.memory, ets as u32 + offset)), [0, ETS_TAG, 0, 0, handle]);
    assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[ets, 0, 0]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[ets, 33, 44]), 1);
    assert_eq!(get(&io.memory, ets as u32 + 16), handle, "second setfn keeps the handle");

    let bytes = io.memory.bytes(ets as u32, ETS_BYTES).unwrap().to_vec();
    io.memory.bytes_mut(copy as u32, ETS_BYTES).unwrap().copy_from_slice(&bytes);
    let ordinary = call(&mut run, &mut io, 0, "malloc", &[36]) as u32;
    assert_ne!(ordinary, 0);
    io.memory.bytes_mut(ordinary, 36).unwrap().fill(0);
    put(&mut io.memory, ordinary, copy as u32); // The key matches, but this block has no node tag.
    for (i, (bad, export, args)) in [
        (handle, "timer_disarm", &[copy][..]), // Valid node, different ETSTimer.
        (ordinary, "timer_setfn", &[copy, 55, 66][..]),
        (0xffff_fff8, "timer_done", &[copy][..]),
        (handle + 1, "timer_arm_us", &[copy, 0, 0][..]),
        (HEAP + HEAP_BYTES - 8, "timer_arm", &[copy, 0, 0][..]),
        (HEAP, "timer_disarm", &[copy][..]),
    ].into_iter().enumerate() {
        io.clock = i as u64 + 1;
        put(&mut io.memory, copy as u32 + 16, bad);
        assert_eq!(call(&mut run, &mut io, 0, export, args), 0, "{export} refuses invalid handle {bad:#x}");
        assert_eq!(call(&mut run, &mut io, 0, "timer_invalid", &[]), i as i32 + 1, "exact invalid count");
        assert_eq!(get(&io.memory, copy as u32 + 4), ETS_TAG, "invalid call preserves tag");
        assert_eq!(get(&io.memory, copy as u32 + 16), bad, "invalid call preserves handle");
    }
    due(&mut run, &mut io, Some((11, 22)), u64::MAX, "original still fires with its first callback");
    assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[ets]), 1);
    assert_eq!(get(&io.memory, ets as u32 + 4), 0, "done clears the tag");
    assert_eq!(get(&io.memory, ets as u32 + 16), 0, "done clears the handle");
    assert_eq!(call(&mut run, &mut io, 0, "timer_disarm", &[ets]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "timer_arm", &[ets, 1, 0]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "timer_invalid", &[]), 6);
    assert_eq!(call(&mut run, &mut io, 0, "timer_unset", &[]), 1, "only arm reports an unset timer");
    assert_eq!(call(&mut run, &mut io, 0, "timer_failures", &[]), 0, "invalid handles are counted separately");
    call(&mut run, &mut io, 0, "free", &[ordinary as i32]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    drop(io);
    assert_eq!(host.reports, [(1, FAULT_KERNEL_TIMER_HANDLE_INVALID, copy), (2, FAULT_KERNEL_TIMER_HANDLE_INVALID, copy),
        (4, FAULT_KERNEL_TIMER_HANDLE_INVALID, copy), (6, FAULT_KERNEL_TIMER_UNSET, ets)]);
}

#[test]
fn timer_node_refusal_returns_its_new_chunk() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(with_timer_storage(memory(), 1), RADIO_KERNEL_LINES, &mut host);
    let key = timer_key(0);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    // Leave exactly one free block with a 256-byte payload and a 16-byte header.
    let held = call(&mut run, &mut io, 0, "malloc", &[initial - 256 - 16]);
    assert_ne!(held, 0);
    let before = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    assert_eq!(before, 256, "the chunk fits and consumes the entire free block");
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[key, 2, 3]), 0, "the node cannot fit");
    assert_eq!(call(&mut run, &mut io, 0, "timer_slots", &[]), 0, "reservation rolled back");
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), before, "refusal returns the chunk");
    assert_eq!(call(&mut run, &mut io, 0, "timer_failures", &[]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[key]), 1, "done clears the pending timer");
    assert_eq!(timer_words(&io, key), [0; 5]);
    call(&mut run, &mut io, 0, "free", &[held]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    check_heap(&io, 0, 0);
    let clock = io.clock;
    drop(io);
    assert_eq!(host.reports, [(clock, FAULT_KERNEL_TIMER_REFUSED, 0)]);
}

#[derive(Clone, Copy)]
struct TimerModel { handle: u32, func: u32, arg: u32, deadline: Option<u64>, period: u64, order: u32 }
impl TimerModel {
    fn installed(io: &KernelPorts<'_>, key: i32, func: u32, arg: u32) -> Self {
        let handle = get(&io.memory, key as u32 + 16);
        assert_ne!(handle, 0, "installation publishes a handle");
        Self { handle, func, arg, deadline: None, period: 0, order: 0 }
    }
}
fn model_due(model: &mut [Option<TimerModel>], now: u64, order: &mut u32) -> (Option<(u32, u32)>, u64) {
    let first = model.iter().enumerate().filter_map(|(i, node)| {
        let node = node.as_ref()?;
        Some((i, node.deadline?, node.order))
    }).min_by_key(|&(_, deadline, sequence)| (deadline, sequence));
    let mut result = None;
    if let Some((index, deadline, _)) = first {
        if deadline <= now {
            let node = model[index].as_mut().unwrap();
            result = Some((node.func, node.arg));
            node.deadline = if node.period == 0 { None } else {
                *order += 1;
                node.order = *order;
                let next = deadline + node.period;
                Some(if next < now { now + node.period } else { next })
            };
        }
    }
    let next = model.iter().flatten().filter_map(|node| node.deadline).min().unwrap_or(u64::MAX);
    (result, next)
}

#[derive(Clone, Copy)]
enum TimerOp { Setfn(u32, u32), ArmUs(u32, u32), Arm(u32, u32), Disarm, Done, Due }

fn random_timer_op(rng: &mut Rng, delay: impl FnOnce(&mut Rng) -> u32) -> TimerOp {
    match rng.next() % 6 {
        0 => TimerOp::Setfn(rng.next(), rng.next()),
        op @ (1 | 2) => {
            let raw = delay(rng);
            let repeat = rng.next() & 1;
            if op == 1 { TimerOp::ArmUs(raw, repeat) } else { TimerOp::Arm(raw, repeat) }
        }
        3 => TimerOp::Disarm,
        4 => TimerOp::Done,
        _ => TimerOp::Due,
    }
}

fn check_timer_model<const KEYS: usize>(
    run: &mut Run<'_>, io: &mut KernelPorts<'_>, layout: &KernelLayout, seed: u32,
    initial_room: usize, operations: impl ExactSizeIterator<Item = (usize, u32, TimerOp)>,
) -> Vec<(u64, u8, i32)> {
    let check_model_heap = |io: &KernelPorts<'_>, step: usize| {
        assert_eq!(heap_status_for_layout(&io.memory, layout), 0, "seed {seed:#x}, operation {step}: heap invariant");
    };
    let initial = call(run, io, 0, "heap_free_bytes", &[]);
    let mut model = [None::<TimerModel>; KEYS];
    let mut pending = [None; KEYS];
    let mut reports = Vec::new();
    let (mut refused, mut lost, mut unset) = (0u32, 0u32, 0u32);
    let mut room = initial_room;
    let held = if room < KEYS {
        // Leave exactly one chunk and this many node blocks at the end of a fresh heap.
        let bytes = radio::kernel::timer_floor_bytes(room as u32) as i32;
        let p = call(run, io, 0, "malloc", &[initial - bytes]);
        assert_ne!(p, 0);
        p
    } else { 0 };
    let mut order = 0;
    let steps = operations.len();
    io.clock = 0x3_ffff_fff0;
    for (step, (index, elapsed, op)) in operations.enumerate() {
        let key = timer_key(index);
        io.clock += u64::from(elapsed);
        let scenario = format!("seed {seed:#x}, operation {step}");
        if step == steps / 2 && held != 0 {
            call(run, io, 0, "free", &[held]);
            room = KEYS;
        }
        let live = model.iter().flatten().count();
        match op {
            TimerOp::Setfn(func, arg) => {
                let new = model[index].is_none() && pending[index].is_none();
                let fits = !new || live < room;
                assert_eq!(call(run, io, 0, "timer_setfn", &[key, func as i32, arg as i32]), i32::from(fits), "{scenario}");
                if new && fits {
                    model[index] = Some(TimerModel::installed(io, key, func, arg));
                } else if new {
                    pending[index] = Some((func, arg));
                    refused += 1;
                    if refused.is_power_of_two() { reports.push((io.clock, FAULT_KERNEL_TIMER_REFUSED, live as i32)); }
                }
            }
            TimerOp::ArmUs(raw, repeat) | TimerOp::Arm(raw, repeat) => {
                let micros = matches!(op, TimerOp::ArmUs(..));
                let name = if micros { "timer_arm_us" } else { "timer_arm" };
                let fits = model[index].is_some() || (pending[index].is_some() && live < room);
                assert_eq!(call(run, io, 0, name, &[key, raw as i32, repeat as i32]), i32::from(fits), "{scenario}");
                if let Some((func, arg)) = pending[index] {
                    if fits {
                        pending[index] = None;
                        model[index] = Some(TimerModel::installed(io, key, func, arg));
                    } else {
                        lost += 1;
                        if lost.is_power_of_two() { reports.push((io.clock, FAULT_KERNEL_TIMER_LOST, key)); }
                    }
                } else if !fits {
                    unset += 1;
                    if unset.is_power_of_two() { reports.push((io.clock, FAULT_KERNEL_TIMER_UNSET, key)); }
                }
                if let Some(node) = &mut model[index] {
                    let us = if micros { u64::from(raw) } else { (u64::from(raw) * 1000).min(u64::from(u32::MAX)) };
                    let ticks = us * u64::from(TICKS_PER_US);
                    order += 1;
                    node.order = order;
                    node.deadline = Some(io.clock + ticks);
                    node.period = if repeat == 0 { 0 } else { ticks };
                }
            }
            TimerOp::Disarm => {
                assert_eq!(call(run, io, 0, "timer_disarm", &[key]), i32::from(model[index].is_some()), "{scenario}");
                if let Some(node) = &mut model[index] { node.deadline = None; }
            }
            TimerOp::Done => {
                let known = model[index].take().is_some() || pending[index].take().is_some();
                assert_eq!(call(run, io, 0, "timer_done", &[key]), i32::from(known), "{scenario}");
            }
            TimerOp::Due => {
                let (expected, next) = model_due(&mut model, io.clock, &mut order);
                due(run, io, expected, next, &scenario);
            }
        }
        // Check externally visible ordering after every mutation too.
        let (expected, next) = model_due(&mut model, io.clock, &mut order);
        due(run, io, expected, next, &scenario);
        let words = match (model[index], pending[index]) {
            (Some(node), None) => [0, ETS_TAG, 0, 0, node.handle],
            (None, Some((func, arg))) => [0, ETS_PENDING_TAG, 0, func, arg],
            (None, None) => [0; 5],
            _ => unreachable!(),
        };
        assert_eq!(timer_words(io, key), words, "{scenario}: ETSTimer lifetime");
        for (name, count) in [("timer_failures", refused), ("timer_lost", lost), ("timer_unset", unset)] {
            assert_eq!(call(run, io, 0, name, &[]), count as i32, "{scenario}: {name}");
        }
        let live = model.iter().flatten().count() as i32;
        let slots = call(run, io, 0, "timer_slots", &[]);
        assert_eq!(slots % 64, 0, "{scenario}: whole chunks");
        assert!((live..=live + 127).contains(&slots), "{scenario}: {live} live, {slots} slots");
        if live == 0 { assert_eq!(slots, 0, "{scenario}: empty table"); }
        if step % 32 == 0 { check_model_heap(io, step); }
    }
    for (index, node) in model.into_iter().enumerate() {
        let key = timer_key(index);
        assert_eq!(call(run, io, 0, "timer_done", &[key]), i32::from(node.is_some() || pending[index].is_some()));
    }
    due(run, io, None, u64::MAX, "fuzz drained");
    assert_eq!(call(run, io, 0, "timer_invalid", &[]), 0, "all model handles valid");
    assert_eq!(call(run, io, 0, "timer_slots", &[]), 0, "drain returns all chunks");
    assert_eq!(call(run, io, 0, "heap_free_bytes", &[]), initial);
    check_model_heap(io, steps);
    reports
}

#[test]
fn timer_fuzz_seed_a341316c() {
    const SEED: u32 = 0xa341_316c;
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(with_timer_storage(memory(), 8), RADIO_KERNEL_LINES, &mut host);
    let mut rng = Rng(SEED);
    let operations = (0..1000).map(|_| {
        let index = rng.next() as usize % 8;
        let elapsed = rng.next() % 97;
        let op = random_timer_op(&mut rng, |rng| if rng.next() & 15 == 0 { u32::MAX } else { rng.next() % 20 });
        (index, elapsed, op)
    });
    let reports = check_timer_model::<8>(&mut run, &mut io, &layout(), SEED, 3, operations);
    drop(io);
    assert_eq!(host.reports, reports);
}

#[test]
fn report_storms_stay_bounded() {
    use fab_elec_mcu_emit_s3::EntryKind;
    use fab_elec_mcu_emu::bus::{Bus, Kind, Memory, Perm, Width};
    use fab_elec_mcu_s3::config::s3_config;
    use fab_elec_mcu_xtensa::sim::Cpu;

    const FAILURES: u32 = 1000;
    const STATE: u32 = 0x18000;
    const SHARED: u32 = 0x1a000;
    const STACK: u32 = 0x20000;
    const RETURN: u32 = 0x100;
    let module = load_kernel(&compiled_objects().module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let mut regions = regions();
    regions.push(KernelRegion { base: ETS_BASE, len: (FAILURES + 1) * ETS_BYTES as u32, writable: true, kind: RegionKind::Pack });
    let options = KernelOptions { base: 0x4001_0000, state: STATE, t0: 0x1c000, reports: Some(REPORTS), stop_pending: Some(STOP_PENDING), regions };
    let code = build_kernel(&module, &options).unwrap();
    let mut mem = Memory::new();
    let ram = mem.store(STACK as usize);
    mem.map(0, ram, 0, STACK as usize, Perm { read: true, write: true, exec: false });
    let text = mem.store(code.bytes.len() + 4);
    mem.bytes_mut(text)[..code.bytes.len()].copy_from_slice(&code.bytes);
    mem.map(code.base, text, 0, code.bytes.len() + 4, Perm { read: true, write: false, exec: true });
    let read = |mem: &mut Memory, at| mem.read(at, Width::W, Kind::Load, 0).unwrap();
    let write = |mem: &mut Memory, at, value| mem.write(at, Width::W, value, 0).unwrap();
    for (at, words) in [(STATE, module.state_init()), (SHARED, module.shared())] {
        for (i, &value) in words.iter().enumerate() { write(&mut mem, at + 4 * i as u32, value); }
    }
    // Run each export against the same RAM, including the emitted report port's ring.
    let invoke = |mem: &mut Memory, name: &str, args: &[u32]| {
        let entry = code.entries.iter().find(|e| e.name == name).unwrap();
        let EntryKind::Export { params, result } = &entry.kind else { panic!("{name}: expected export") };
        assert_eq!(args.len(), params.len());
        for (&at, &arg) in params.iter().zip(args) { write(mem, at, arg); }
        let mut cpu = Cpu::new(s3_config(), code.base + entry.offset);
        cpu.ps = 15;
        cpu.ar[..4].copy_from_slice(&[RETURN, STACK, STATE, SHARED]);
        for _ in 0..entry.max_insns {
            cpu.step(mem).unwrap();
            if cpu.pc == RETURN { return read(mem, *result); }
        }
        panic!("{name}: exceeded emitted instruction bound")
    };
    assert!(mem.bytes(ram)[HEAP as usize..(HEAP + HEAP_BYTES) as usize].iter().all(|&b| b == 0),
        "report_storms_stay_bounded must supply the zeroed heap required by objects.splash");
    let initial = invoke(&mut mem, "heap_free_bytes", &[]);
    let held = invoke(&mut mem, "malloc", &[initial]);
    assert_ne!(held, 0);
    let unset = timer_key(FAILURES as usize) as u32;
    for (counter, fault) in [("timer_failures", FAULT_KERNEL_TIMER_REFUSED), ("timer_unset", FAULT_KERNEL_TIMER_UNSET),
        ("heap_failures", FAULT_KERNEL_HEAP_EXHAUSTED)] {
        let start = read(&mut mem, REPORTS + 4);
        let mut records = Vec::new();
        for n in 1..=FAILURES {
            let arg = match fault {
                FAULT_KERNEL_TIMER_REFUSED => {
                    assert_eq!(invoke(&mut mem, "timer_setfn", &[timer_key((n - 1) as usize) as u32, n, n]), 0);
                    0
                }
                FAULT_KERNEL_TIMER_UNSET => { assert_eq!(invoke(&mut mem, "timer_arm", &[unset, 1, 0]), 0); unset }
                FAULT_KERNEL_HEAP_EXHAUSTED => { assert_eq!(invoke(&mut mem, "malloc", &[1]), 0); 1 }
                _ => unreachable!(),
            };
            let tail = read(&mut mem, REPORTS + 4);
            assert_eq!(tail - start, n.ilog2() + 1, "{counter}: publication at powers of two");
            if n == 128 || n == FAILURES {
                // Consume each record and advance the head exactly as the dispatcher does.
                let mut head = read(&mut mem, REPORTS);
                while head != tail {
                    let at = REPORTS + 12 + (head % radio::REPORT_RING_ENTRIES) * 12;
                    records.push((read(&mut mem, at), read(&mut mem, at + 8)));
                    head += 1;
                    write(&mut mem, REPORTS, head);
                }
                assert_eq!(read(&mut mem, REPORTS + 8), 0, "{counter}: no dropped reports");
                assert!(records.iter().all(|&record| record == (u32::from(fault), arg)));
            }
        }
        assert_eq!(invoke(&mut mem, counter, &[]), FAILURES);
        assert_eq!(records.len(), 10);
    }
    assert_eq!(invoke(&mut mem, "timer_lost", &[]), 0);
    invoke(&mut mem, "free", &[held]);
}

fn reserved_layout() -> KernelLayout {
    KernelLayout { heap_size: 16 * 1024, timer_reserve: 64, ..layout() }
}
fn reserved_objects() -> &'static Compiled {
    static SOURCE: std::sync::OnceLock<Compiled> = std::sync::OnceLock::new();
    SOURCE.get_or_init(|| compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES, &kernel_consts(&reserved_layout()))
        .unwrap_or_else(|errors| panic!("timer reserve layout: {errors:?}")))
}
fn reserved_ports(host: &mut Host, keys: usize) -> KernelPorts<'_> {
    let layout = reserved_layout();
    let mut memory = KernelMemory::new();
    for (base, bytes) in [(layout.heap_base, layout.heap_size), (CURRENT, 4), (TASKS, (TASKS_MAX + 1) * 4),
        (DEADLINE_OUT - 4, 24)] {
        memory.add_region(base, vec![0; bytes as usize], true, RegionKind::Kernel).unwrap();
    }
    assert!(memory.bytes(layout.heap_base, layout.heap_size as usize).unwrap().iter().all(|&b| b == 0),
        "reserved_ports must supply the zeroed heap required by objects.splash");
    KernelPorts::new(with_timer_storage(memory, keys), RADIO_KERNEL_LINES, host)
}

fn malloc_to_floor(run: &mut Run<'_>, io: &mut KernelPorts<'_>) -> (i32, i32) {
    let initial = call(run, io, 0, "heap_free_bytes", &[]);
    let floor = radio::kernel::timer_floor_bytes(reserved_layout().timer_reserve) as i32;
    let held = call(run, io, 0, "malloc", &[initial - floor - 16]);
    assert_ne!(held, 0);
    assert_eq!(call(run, io, 0, "heap_free_bytes", &[]), floor, "a split can leave exactly the floor");
    (held, initial)
}
fn use_reserved_timers(run: &mut Run<'_>, io: &mut KernelPorts<'_>) {
    for i in 0..reserved_layout().timer_reserve as usize {
        assert_eq!(call(run, io, 0, "timer_setfn", &[timer_key(i), i as i32 + 1, i as i32]), 1);
    }
}

#[test]
fn timer_floor_keeps_timers_settable() {
    let count = reserved_layout().timer_reserve as usize;
    let mut run = Run::new(reserved_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = reserved_ports(&mut host, count);
    let (held, initial) = malloc_to_floor(&mut run, &mut io);
    assert_eq!(call(&mut run, &mut io, 0, "malloc", &[1]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "heap_failures", &[]), 1);
    use_reserved_timers(&mut run, &mut io);
    for i in 0..count { assert_eq!(call(&mut run, &mut io, 0, "timer_arm_us", &[timer_key(i), 0, 0]), 1); }
    for i in 0..count {
        due(&mut run, &mut io, Some((i as u32 + 1, i as u32)), if i + 1 == count { u64::MAX } else { 0 }, "reserve FIFO");
        assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[timer_key(i)]), 1);
    }
    call(&mut run, &mut io, 0, "free", &[held]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    drop(io);
    assert_eq!(host.reports, [(0, FAULT_KERNEL_HEAP_EXHAUSTED, 1)]);
}

#[test]
fn object_allocations_and_in_place_growth_honor_the_floor() {
    let mut run = Run::new(reserved_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = reserved_ports(&mut host, 1);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    let isolated = call(&mut run, &mut io, 0, "malloc", &[40]);
    assert_ne!(isolated, 0);
    let (held, _) = malloc_to_floor(&mut run, &mut io);
    let size = call(&mut run, &mut io, 0, "heap_request_bytes", &[held]);
    let floor = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    // Reusing an isolated block needs only its payload's worth of free bytes.
    call(&mut run, &mut io, 0, "free", &[isolated]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), floor + 40);
    assert_eq!(call(&mut run, &mut io, 0, "malloc", &[40]), isolated);

    for (index, (name, args)) in [
        ("realloc_reserve", &[held, size + 8][..]), ("calloc_reserve", &[3, 17]),
        ("queue_create", &[2, 4]), ("osi_wifi_create_queue", &[2, 4]),
        ("sem_create", &[1, 0]), ("mutex_create", &[0]), ("task_create", &[TASK_EXIT as i32, 0, 0, 0, 1, 0]),
        ("calloc_reserve", &[0xffff, 0x10000]),
    ].into_iter().enumerate() {
        assert_eq!(call(&mut run, &mut io, 0, name, args), 0, "{name}: protects the floor");
        assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), floor, "{name}: no bytes consumed");
        assert_eq!(call(&mut run, &mut io, 0, "heap_failures", &[]), index as i32 + 1);
    }
    assert_eq!(call(&mut run, &mut io, 0, "heap_request_bytes", &[held]), size, "failed growth keeps the request");
    assert_eq!(heap_status_for_layout(&io.memory, &reserved_layout()), 0);
    call(&mut run, &mut io, 0, "free", &[held]);
    call(&mut run, &mut io, 0, "free", &[isolated]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    drop(io);
    assert_eq!(host.reports, [size + 8, 51, 8, 0xffff0000u32 as i32].map(|bytes| (0, FAULT_KERNEL_HEAP_EXHAUSTED, bytes)));
}

fn check_pending_arm(attempts_without_memory: usize) {
    let count = reserved_layout().timer_reserve as usize;
    let key = timer_key(count);
    let mut run = Run::new(reserved_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = reserved_ports(&mut host, count + 1);
    let (held, initial) = malloc_to_floor(&mut run, &mut io);
    use_reserved_timers(&mut run, &mut io);
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[key, 31, 47]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[key, 41, 53]), 1, "pending keeps the first callback");
    assert_eq!(call(&mut run, &mut io, 0, "timer_disarm", &[key]), 0);
    let pending = [0, ETS_PENDING_TAG, 0, 31, 47];
    assert_eq!(timer_words(&io, key), pending);
    let mut expected = vec![(0, FAULT_KERNEL_TIMER_REFUSED, count as i32)];
    for n in 1..=attempts_without_memory {
        io.clock = n as u64;
        let arm = if n % 2 == 0 { "timer_arm" } else { "timer_arm_us" };
        assert_eq!(call(&mut run, &mut io, 0, arm, &[key, 1, 0]), 0);
        assert_eq!(call(&mut run, &mut io, 0, "timer_lost", &[]), n as i32);
        assert_eq!(timer_words(&io, key), pending);
        if n.is_power_of_two() { expected.push((io.clock, FAULT_KERNEL_TIMER_LOST, key)); }
    }
    call(&mut run, &mut io, 0, "free", &[held]);
    assert_eq!(call(&mut run, &mut io, 0, "timer_arm", &[key, 0, 0]), 1);
    let [_, tag, _, func, handle] = timer_words(&io, key);
    assert_eq!((tag, func), (ETS_TAG, 0));
    assert!((HEAP..HEAP + reserved_layout().heap_size).contains(&handle));
    due(&mut run, &mut io, Some((31, 47)), u64::MAX, "retry fires the saved callback");
    for i in 0..=count { assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[timer_key(i)]), 1); }
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    for (name, value) in [("timer_failures", 1), ("timer_lost", attempts_without_memory as i32),
        ("timer_unset", 0), ("timer_invalid", 0), ("heap_failures", 0)] {
        assert_eq!(call(&mut run, &mut io, 0, name, &[]), value, "{name}");
    }
    drop(io);
    assert_eq!(host.reports, expected);
}

#[test]
fn refused_timer_stays_pending_until_its_arm() { check_pending_arm(0); }

#[test]
fn pending_arm_without_memory_is_lost_and_reported() { check_pending_arm(5); }

#[test]
fn timer_heap_multiple_chunks_and_tied_deadlines() {
    const SEED: u32 = 0x531a_8d47;
    const KEYS: usize = 160;
    let large = reserved_layout();
    let mut run = Run::new(reserved_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = reserved_ports(&mut host, KEYS);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    for i in 0..65 {
        let key = timer_key(i);
        assert_eq!(call(&mut run, &mut io, 0, "timer_setfn", &[key, key, 0]), 1);
        assert_eq!(call(&mut run, &mut io, 0, "timer_slots", &[]), if i < 64 { 64 } else { 128 });
    }
    for i in (1..65).rev() { assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[timer_key(i)]), 1); }
    assert_eq!(call(&mut run, &mut io, 0, "timer_slots", &[]), 128, "127 spare slots with one timer left");
    assert_eq!(call(&mut run, &mut io, 0, "timer_done", &[timer_key(0)]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "timer_slots", &[]), 0, "last done releases both chunks");
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial, "all 65 nodes and both chunks returned");

    // Cross the H_SEQ rollover during the first group of tied deadlines.
    put(&mut io.memory, large.heap_base + 56, u32::MAX - 63);
    let mut rng = Rng(SEED);
    let initially_armed = 150;
    let operations = (0..3000).map(|step| {
        // Register all keys, then arm across three chunks at four distinct deadlines.
        if step < KEYS { return (step, 0, TimerOp::Setfn(rng.next(), rng.next())); }
        if step < KEYS + initially_armed { return (step - KEYS, 0, TimerOp::ArmUs(1 + rng.next() % 4, 0)); }
        if step == KEYS + initially_armed { return (0, 4 * TICKS_PER_US, TimerOp::Due); }
        let index = rng.next() as usize % KEYS;
        let elapsed = if rng.next() % 16 == 0 { 1000 * TICKS_PER_US } else { 0 };
        let op = random_timer_op(&mut rng, |rng| rng.next() % 4);
        (index, elapsed, op)
    });
    let reports = check_timer_model::<KEYS>(&mut run, &mut io, &large, SEED, KEYS, operations);
    drop(io);
    assert_eq!(host.reports, reports);
}

#[derive(Clone, Copy, Debug)]
enum Object { Receiver, BackSender, FrontSender, Semaphore, Mutex }
impl Object {
    fn create(self, run: &mut Run<'_>, io: &mut KernelPorts<'_>) -> i32 {
        let object = match self {
            Self::Receiver | Self::BackSender | Self::FrontSender => call(run, io, 0, "queue_create", &[1, 4]),
            Self::Semaphore => call(run, io, 0, "sem_create", &[2, 0]),
            Self::Mutex => call(run, io, 0, "mutex_create", &[0]),
        };
        assert_ne!(object, 0, "{self:?}: create");
        match self {
            Self::BackSender | Self::FrontSender => {
                put(&mut io.memory, buffer(0) as u32, 99);
                assert_eq!(call(run, io, 0, "queue_send_back", &[object, buffer(0), 0]), 1);
            }
            Self::Mutex => { assert_eq!(call(run, io, 0, "mutex_lock", &[object, 0]), 1); }
            _ => {}
        }
        object
    }
    fn wait(self, run: &mut Run<'_>, io: &mut KernelPorts<'_>, object: i32, task: u32, timeout: i32) -> i32 {
        match self {
            Self::Receiver => call(run, io, task, "queue_recv", &[object, buffer(task), timeout]),
            Self::BackSender => call(run, io, task, "queue_send_back", &[object, buffer(task), timeout]),
            Self::FrontSender => call(run, io, task, "queue_send_front", &[object, buffer(task), timeout]),
            Self::Semaphore => call(run, io, task, "sem_take", &[object, timeout]),
            Self::Mutex => call(run, io, task, "mutex_lock", &[object, timeout]),
        }
    }
    fn delete(self, run: &mut Run<'_>, io: &mut KernelPorts<'_>, object: i32) {
        let name = match self { Self::Semaphore => "sem_delete", Self::Mutex => "mutex_delete", _ => "queue_delete" };
        call(run, io, 0, name, &[object]);
    }
}

#[test]
fn priority_fifo_and_completed_operations_table() {
    let mut run = Run::new(compiled_objects());
    for (scenario, kind, wake_order) in [
        ("receiver direct handoff", Object::Receiver, [2, 3, 4, 1]),
        ("full queue sender refill", Object::BackSender, [2, 3, 4, 1]),
        ("semaphore token handoff", Object::Semaphore, [2, 3, 4, 1]),
        ("mutex ownership handoff", Object::Mutex, [2, 3, 4, 1]),
    ] {
        let mut host = Host::default();
        let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
        let object = kind.create(&mut run, &mut io);
        for task in 1..=4 {
            put(&mut io.memory, buffer(task) as u32, 100 + task);
            assert_eq!(kind.wait(&mut run, &mut io, object, task, -1), 0, "{scenario}: wait {task}");
            assert_eq!(get64(&io.memory, tcb(task) + TCB_DEADLINE), u64::MAX, "{scenario}: forever");
            assert!(io.raised.is_empty(), "{scenario}: blocking does not wake a task");
        }
        waiters(&io, &wake_order, scenario);
        let mut owner = 0;
        let mut queued = 99;
        for (index, &task) in wake_order.iter().enumerate() {
            let caller = if matches!(kind, Object::Mutex) { owner } else { 0 };
            let result = match kind {
                Object::Receiver => {
                    put(&mut io.memory, buffer(0) as u32, 500 + task);
                    call(&mut run, &mut io, caller, "queue_send_back", &[object, buffer(0), 0])
                }
                Object::BackSender => call(&mut run, &mut io, caller, "queue_recv", &[object, buffer(0), 0]),
                Object::Semaphore => call(&mut run, &mut io, caller, "sem_give", &[object]),
                Object::Mutex => call(&mut run, &mut io, caller, "mutex_unlock", &[object]),
                _ => unreachable!(),
            };
            assert_eq!(result, 1, "{scenario}: wake {task}");
            completed(&io, task, 1, scenario);
            let expected_irq = if PRIORITIES[task as usize] > PRIORITIES[caller as usize] { vec![YIELD_LINE] } else { vec![] };
            assert_eq!(io.raised, expected_irq, "{scenario}: exact yield requests for task {task}");
            waiters(&io, &wake_order[index + 1..], scenario);
            match kind {
                Object::Receiver => {
                    assert_eq!(get(&io.memory, buffer(task) as u32), 500 + task, "{scenario}: receiver buffer written");
                    assert_eq!(call(&mut run, &mut io, 0, "queue_waiting", &[object]), 0, "direct handoff uses no slot");
                }
                Object::BackSender => {
                    assert_eq!(get(&io.memory, buffer(0) as u32), queued, "{scenario}: old queued value");
                    queued = 100 + task;
                    assert_eq!(call(&mut run, &mut io, 0, "queue_waiting", &[object]), 1, "sender refills the freed slot");
                }
                Object::Semaphore => assert_eq!(call(&mut run, &mut io, 0, "sem_count", &[object]), 0, "waiter consumed token"),
                Object::Mutex => owner = task,
                _ => unreachable!(),
            }
        }
        if matches!(kind, Object::Mutex) { assert_eq!(call(&mut run, &mut io, owner, "mutex_unlock", &[object]), 1); }
        if matches!(kind, Object::BackSender) {
            assert_eq!(call(&mut run, &mut io, 0, "queue_recv", &[object, buffer(0), 0]), 1);
            assert_eq!(get(&io.memory, buffer(0) as u32), queued);
        }
        kind.delete(&mut run, &mut io, object);
        assert_eq!(heap_status(&io.memory), 0, "{scenario}: heap after deletion");
    }
}

#[test]
fn event_posts_copy_in_the_poster_and_transfer_heap_ownership_in_fifo_order() {
    use fab_elec_mcu_emit32::xtensa::{assemble, reg::*, Asm};
    use fab_elec_mcu_emu::bus::{Memory, Perm};
    use fab_elec_mcu_s3::config::s3_config;
    use fab_elec_mcu_xtensa::sim::Cpu;
    use radio::{event_glue::event_post_glue, kernel::gates::{gates, EntryRegs}};

    const RAM_END: u32 = 0x3_0000;
    const TEXT: u32 = 0x4008_0000;
    const CALLER: u32 = 0x4000_1000;
    const STATE: u32 = 0x1_8000;
    const SHARED: u32 = 0x1_a000;
    let layout = KernelLayout { heap_base: 0x2_0000, heap_size: 16 * 1024, ..layout() };
    let source = compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES, &kernel_consts(&layout)).unwrap();
    let module = load_kernel(&source.module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
    let ram_region = KernelRegion { base: CURRENT, len: RAM_END - CURRENT, writable: true, kind: RegionKind::Kernel };
    let options = KernelOptions { base: TEXT, state: STATE, t0: 0x1_c000, reports: Some(REPORTS), stop_pending: Some(STOP_PENDING),
        regions: [ram_region].into_iter().chain(device_regions()).collect() };
    let code = build_kernel(&module, &options).unwrap();
    let gate_base = (TEXT + code.bytes.len() as u32).next_multiple_of(4);
    let gates = gates(gate_base, &code, EntryRegs { state: STATE, shared: SHARED });
    let names = ["malloc", "event_commit", "event_take", "free", "heap_free_bytes"];
    let imports: Vec<_> = names.iter().map(|&name| {
        let (index, gate) = gates.gates.iter().enumerate().find(|(_, g)| g.name == name).unwrap();
        radio::Gate { name, call: gate.address, blocks: gate.blocks, exit_level: None, export: index as u16 }
    }).collect();
    let thunk_base = (gate_base + gates.bytes.len() as u32).next_multiple_of(4);
    let thunks = radio::exports(thunk_base, &[], &imports, None, None);
    let target = |name: &str| thunks.thunks[names.iter().position(|&n| n == name).unwrap()];
    let mut glue = event_post_glue();
    for reloc in &glue.relocations {
        let at = reloc.offset as usize;
        glue.bytes[at..at + 4].copy_from_slice(&target(&reloc.symbol).to_le_bytes());
    }
    let glue_base = (thunk_base + thunks.bytes.len() as u32).next_multiple_of(4);
    let post = glue_base + glue.entry;
    let mut done = 0;
    let caller = assemble(&mut Asm::new(0, A8), |a| {
        a.entry(A1, 32); a.callx8(A8); done = CALLER + a.base.offset(); a.op24(0);
    });
    let mut mem = Memory::new();
    let ram = mem.store(RAM_END as usize);
    mem.map(0, ram, 0, RAM_END as usize, Perm { read: true, write: true, exec: false });
    for (addr, bytes) in [(TEXT, &code.bytes), (gate_base, &gates.bytes), (thunk_base, &thunks.bytes),
        (glue_base, &glue.bytes), (CALLER, &caller)] {
        let len = bytes.len().next_multiple_of(4);
        let store = mem.store(len);
        mem.bytes_mut(store)[..bytes.len()].copy_from_slice(bytes);
        mem.map(addr, store, 0, len, Perm { read: true, write: false, exec: true });
    }
    for (addr, words) in [(STATE, module.state_init()), (SHARED, module.shared())] {
        for (i, &word) in words.iter().enumerate() {
            let at = addr as usize + 4 * i;
            mem.bytes_mut(ram)[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    let word = |m: &Memory, addr: u32| u32::from_le_bytes(m.bytes(ram)[addr as usize..addr as usize + 4].try_into().unwrap());
    let task = tcb(1);
    mem.bytes_mut(ram)[CURRENT as usize..CURRENT as usize + 4].copy_from_slice(&task.to_le_bytes());
    let state = (task + TCB_STATE) as usize;
    mem.bytes_mut(ram)[state..state + 4].copy_from_slice(&READY.to_le_bytes());
    let gate_budget: u64 = gates.gates.iter().filter(|g| names.contains(&g.name.as_str())).map(|g| g.max_insns).sum();
    let invoke = |mem: &mut Memory, entry, args: &[u32]| {
        let mut cpu = Cpu::new(s3_config(), CALLER);
        cpu.ps = (1 << 18) | (1 << 5); cpu.window_base = 15; cpu.window_start = 1 << 15;
        cpu.cpenable = 1;
        cpu.ar[1] = RAM_END - 16; cpu.ar[8] = entry;
        cpu.ar[10..10 + args.len()].copy_from_slice(args);
        // Each gate runs at most once per call, and a successful copy fits the heap.
        for _ in 0..gate_budget + (thunks.bytes.len() + glue.bytes.len() + caller.len()) as u64 + 6 * u64::from(layout.heap_size) {
            if (post..glue_base + glue.bytes.len() as u32).contains(&cpu.pc) { assert_eq!(cpu.ps & 15, 0); }
            cpu.step(mem).unwrap();
            if cpu.pc == done { return cpu.ar[10]; }
        }
        panic!("windowed call to {entry:#x} exceeded the gate and payload bound");
    };
    let input = 0x9001; // deliberately unaligned, beyond the report ring and below kernel state
    let base = 0x3c00_1234;
    let send = |m: &mut Memory, id, size: usize| {
        m.bytes_mut(ram)[input as usize..input as usize + size].fill(id as u8);
        assert_eq!(invoke(m, post, &[base + id, id, input, size as u32, u32::MAX]), 0);
        assert_eq!(word(m, task + TCB_STATE), READY);
    };
    let take = |m: &mut Memory, id, size| {
        let node = invoke(m, target("event_take"), &[]);
        assert_ne!(node, 0);
        assert_eq!([4, 8, 12].map(|off| word(m, node + off)), [base + id, id, size]);
        let payload = (node + 16) as usize;
        assert!(m.bytes(ram)[payload..payload + size as usize].iter().all(|&b| b == id as u8));
        invoke(m, target("free"), &[node]);
    };
    let available = invoke(&mut mem, target("heap_free_bytes"), &[]);
    assert_eq!(invoke(&mut mem, target("event_take"), &[]), 0);
    send(&mut mem, 1, 5); send(&mut mem, 2, 2048);
    take(&mut mem, 1, 5); send(&mut mem, 3, 17);
    take(&mut mem, 2, 2048); take(&mut mem, 3, 17);
    for id in 0..64 { send(&mut mem, id, 4); }
    for id in 0..64 { take(&mut mem, id, 4); }
    assert_eq!(invoke(&mut mem, target("event_take"), &[]), 0);
    assert_eq!(invoke(&mut mem, target("heap_free_bytes"), &[]), available);

    let held = invoke(&mut mem, target("malloc"), &[available]);
    assert_ne!(held, 0);
    assert_eq!(invoke(&mut mem, post, &[base, 70, 0, 0, 0]), u32::MAX);
    invoke(&mut mem, target("free"), &[held]);
    assert_eq!(invoke(&mut mem, post, &[base, 71, 0, u32::MAX, 0]), u32::MAX);
    assert_eq!(word(&mem, REPORTS + 4), 2);
    for (slot, request) in [(0, 16), (1, u32::MAX)] {
        let record = REPORTS + 12 + 12 * slot;
        assert_eq!((word(&mem, record), word(&mem, record + 8)), (u32::from(FAULT_KERNEL_HEAP_EXHAUSTED), request));
    }
    assert_eq!(invoke(&mut mem, target("event_take"), &[]), 0);
    assert_eq!(invoke(&mut mem, post, &[base + 72, 72, 0, 0, 0]), 0);
    take(&mut mem, 72, 0);
}

#[test]
fn queue_front_back_wrap_and_blocked_front_table() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    for (len, size) in [(0, 4), (1, 0), (1, 65), (1, -1), (-1, 4)] {
        assert_eq!(call(&mut run, &mut io, 0, "queue_create", &[len, size]), 0, "invalid queue {len}/{size}");
    }
    let q = call(&mut run, &mut io, 0, "queue_create", &[3, 4]);
    assert_ne!(q, 0);
    // scenario, operation, item sent/expected, result, resulting item count
    for (scenario, operation, item, result, count) in [
        ("back first", "queue_send_back", 10, 1, 1),
        ("back second", "queue_send_back", 11, 1, 2),
        ("front wraps", "queue_send_front", 9, 1, 3),
        ("full does not block at zero", "queue_send_back", 90, 0, 3),
        ("read front", "queue_recv", 9, 1, 2),
        ("back wraps", "queue_send_back", 12, 1, 3),
        ("read oldest", "queue_recv", 10, 1, 2),
        ("read middle", "queue_recv", 11, 1, 1),
        ("read wrapped back", "queue_recv", 12, 1, 0),
        ("empty does not block at zero", "queue_recv", 0xdead, 0, 0),
    ] {
        put(&mut io.memory, buffer(0) as u32, if operation == "queue_recv" { 0xdead } else { item });
        assert_eq!(call(&mut run, &mut io, 0, operation, &[q, buffer(0), 0]), result, "{scenario}");
        assert_eq!(get(&io.memory, buffer(0) as u32), item, "{scenario}: copied value or untouched output");
        assert_eq!(get(&io.memory, tcb(0) + TCB_STATE), READY, "{scenario}: timeout zero");
        assert_eq!(call(&mut run, &mut io, 0, "queue_waiting", &[q]), count, "{scenario}: queued count");
    }
    for item in [10, 11, 12] {
        put(&mut io.memory, buffer(0) as u32, item);
        assert_eq!(call(&mut run, &mut io, 0, "queue_send_back", &[q, buffer(0), 0]), 1);
    }
    for (task, name, item) in [(2, "queue_send_front", 20), (3, "queue_send_back", 21)] {
        put(&mut io.memory, buffer(task) as u32, item);
        assert_eq!(call(&mut run, &mut io, task, name, &[q, buffer(task), -1]), 0);
        assert_eq!(get(&io.memory, tcb(task) + TCB_WAIT_ARG), buffer(task) as u32, "unaligned buffer retained intact");
    }
    waiters(&io, &[2, 3], "mixed front/back sender FIFO");
    for (index, expected) in [10, 20, 11, 12, 21].into_iter().enumerate() {
        assert_eq!(call(&mut run, &mut io, 0, "queue_recv", &[q, buffer(0), 0]), 1);
        assert_eq!(get(&io.memory, buffer(0) as u32), expected, "blocked front/back receive {index}");
        if index < 2 { completed(&io, index as u32 + 2, 1, "sender operation completed into freed slot"); }
    }
    call(&mut run, &mut io, 0, "queue_delete", &[q]);
    let q = call(&mut run, &mut io, 0, "queue_create", &[1, 64]);
    assert_ne!(q, 0, "QUEUE_ITEM_MAX is accepted");
    let bytes: Vec<u8> = (0..64).map(|n| (n * 3) as u8).collect();
    io.memory.bytes_mut(buffer(0) as u32, 64).unwrap().copy_from_slice(&bytes);
    assert_eq!(call(&mut run, &mut io, 0, "queue_send_back", &[q, buffer(0), 0]), 1);
    assert_eq!(call(&mut run, &mut io, 1, "queue_recv", &[q, buffer(1), 0]), 1);
    assert_eq!(io.memory.bytes(buffer(1) as u32, 64).unwrap(), bytes);
    call(&mut run, &mut io, 0, "queue_delete", &[q]);
    drop(io);
    assert_eq!(host.reports, [(0, FAULT_KERNEL_HEAP_EXHAUSTED, -1)]);
}

#[test]
fn irq_and_isr_woken_table() {
    let mut run = Run::new(compiled_objects());
    for (scenario, kind, regular, isr_name) in [
        ("back send wakes receiver", Object::Receiver, "queue_send_back", "queue_send_back_isr"),
        ("front send wakes receiver", Object::Receiver, "queue_send_front", "queue_send_front_isr"),
        ("receive wakes sender", Object::BackSender, "queue_recv", "queue_recv_isr"),
        ("give wakes taker", Object::Semaphore, "sem_give", "sem_give_isr"),
    ] {
        for (relation, priority, higher) in [("lower", 2, false), ("equal", 3, false), ("higher", 4, true)] {
            for isr in [false, true] {
                let mut host = Host::default();
                let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
                put(&mut io.memory, tcb(0) + TCB_PRIORITY, 3);
                put(&mut io.memory, tcb(1) + TCB_PRIORITY, priority);
                let object = kind.create(&mut run, &mut io);
                put(&mut io.memory, buffer(1) as u32, 101);
                assert_eq!(kind.wait(&mut run, &mut io, object, 1, -1), 0);
                io.memory.bytes_mut(WOKEN - 1, 3).unwrap().copy_from_slice(&[0xa5, 0x7a, 0x5a]);
                put(&mut io.memory, buffer(0) as u32, 501);
                let args = if matches!(kind, Object::Semaphore) {
                    if isr { vec![object, WOKEN as i32] } else { vec![object] }
                } else { vec![object, buffer(0), if isr { WOKEN as i32 } else { 0 }] };
                assert_eq!(call(&mut run, &mut io, 0, if isr { isr_name } else { regular }, &args), 1,
                    "{scenario}/{relation}/isr={isr}");
                completed(&io, 1, 1, scenario);
                assert_eq!(get(&io.memory, tcb(0) + TCB_STATE), READY, "{scenario}: caller never blocks");
                assert_eq!(io.raised, if higher && !isr { vec![YIELD_LINE] } else { vec![] },
                    "{scenario}/{relation}/isr={isr}: exact irq_raise");
                assert_eq!(io.memory.bytes(WOKEN - 1, 3).unwrap(), &[0xa5, if higher && isr { 1 } else { 0x7a }, 0x5a],
                    "{scenario}/{relation}/isr={isr}: only a higher-priority ISR wake stores one byte");
                match kind {
                    Object::Receiver => assert_eq!(get(&io.memory, buffer(1) as u32), 501, "ISR direct handoff"),
                    Object::BackSender => {
                        assert_eq!(get(&io.memory, buffer(0) as u32), 99, "receive old item before sender refill");
                        assert_eq!(call(&mut run, &mut io, 0, "queue_recv", &[object, buffer(0), 0]), 1);
                        assert_eq!(get(&io.memory, buffer(0) as u32), 101, "sender's operation already completed");
                    }
                    Object::Semaphore => assert_eq!(call(&mut run, &mut io, 0, "sem_count", &[object]), 0),
                    _ => unreachable!(),
                }
                kind.delete(&mut run, &mut io, object);
            }
        }
    }
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    let q = call(&mut run, &mut io, 0, "queue_create", &[1, 4]);
    let s = call(&mut run, &mut io, 0, "sem_create", &[1, 0]);
    io.memory.bytes_mut(WOKEN, 1).unwrap()[0] = 1; // already set by another ISR operation
    for (scenario, name, args, expected) in [
        ("empty receive", "queue_recv_isr", vec![q, buffer(0), WOKEN as i32], 0),
        ("empty take", "sem_take_isr", vec![s, WOKEN as i32], 0),
        ("send with room", "queue_send_back_isr", vec![q, buffer(0), WOKEN as i32], 1),
        ("full back send", "queue_send_back_isr", vec![q, buffer(0), WOKEN as i32], 0),
        ("full front send", "queue_send_front_isr", vec![q, buffer(0), WOKEN as i32], 0),
        ("give with room", "sem_give_isr", vec![s, WOKEN as i32], 1),
        ("give at max", "sem_give_isr", vec![s, WOKEN as i32], 0),
        ("take available", "sem_take_isr", vec![s, WOKEN as i32], 1),
    ] {
        assert_eq!(call(&mut run, &mut io, 0, name, &args), expected, "{scenario}");
        assert_eq!(get(&io.memory, tcb(0) + TCB_STATE), READY, "{scenario}: ISR cannot block");
        assert_eq!(get(&io.memory, tcb(0) + TCB_WAIT_OBJ), 0, "{scenario}: ISR cannot enqueue");
        assert_eq!(io.memory.bytes(WOKEN, 1).unwrap(), &[1], "{scenario}: preserve existing woken byte");
        assert!(io.raised.is_empty(), "{scenario}: no ISR raises line 7");
    }
}

#[test]
fn expiry_middle_unlink_zero_and_forever_table() {
    let mut run = Run::new(compiled_objects());
    let start = 0x1234_ffff_fff0u64;
    for (scenario, kind) in [
        ("receiver timeout", Object::Receiver), ("back sender timeout", Object::BackSender),
        ("front sender timeout", Object::FrontSender), ("semaphore timeout", Object::Semaphore),
        ("mutex timeout", Object::Mutex),
    ] {
        let mut host = Host::default();
        let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
        let object = kind.create(&mut run, &mut io);
        io.clock = start;
        assert_eq!(kind.wait(&mut run, &mut io, object, 5, 0), 0, "{scenario}: timeout zero");
        assert_eq!(get(&io.memory, tcb(5) + TCB_STATE), READY, "{scenario}: zero does not block");
        assert_eq!(get(&io.memory, tcb(5) + TCB_WAIT_OBJ), 0, "{scenario}: zero does not link");
        for (task, timeout) in [(1, 30), (2, 50), (3, 10), (4, -1)] {
            assert_eq!(kind.wait(&mut run, &mut io, object, task, timeout), 0, "{scenario}: task {task} waits");
            let expected = if timeout == -1 { u64::MAX } else { start + timeout as u64 * u64::from(TICKS_PER_US) };
            assert_eq!(get64(&io.memory, tcb(task) + TCB_DEADLINE), expected, "{scenario}: 64-bit deadline carry");
        }
        waiters(&io, &[2, 3, 4, 1], scenario);
        for (phase, us, expired, remaining, next) in [
            ("not due", 9, None, vec![2, 3, 4, 1], Some(10)),
            ("middle", 10, Some(3), vec![2, 4, 1], Some(30)),
            ("tail", 30, Some(1), vec![2, 4], Some(50)),
            ("head", 50, Some(2), vec![4], None),
        ] {
            io.clock = start + us * u64::from(TICKS_PER_US);
            assert_eq!(call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]), 0, "{scenario}/{phase}");
            if let Some(task) = expired { completed(&io, task, 0, scenario); }
            assert_eq!(io.raised, if expired.is_some() { vec![YIELD_LINE] } else { vec![] }, "{scenario}/{phase}: timeout wake IRQ");
            waiters(&io, &remaining, scenario);
            let expected = next.map_or(u64::MAX, |us| start + us * u64::from(TICKS_PER_US));
            assert_eq!(get64(&io.memory, DEADLINE_OUT), expected, "{scenario}/{phase}: next deadline");
        }
        io.clock = u64::MAX;
        call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
        waiters(&io, &[4], "forever never expires, even at the sentinel timestamp");
        assert_eq!(get64(&io.memory, DEADLINE_OUT), u64::MAX);
        kind.delete(&mut run, &mut io, object);
        completed(&io, 4, 0, "deletion cancels the remaining waiter before freeing its header");
        assert_eq!(heap_status(&io.memory), 0);
    }
}

#[test]
fn unsigned_timeout_product_and_task_count() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    let s = Object::Semaphore.create(&mut run, &mut io);
    io.clock = 0x7_ffff_fff8;
    let timeout = 0x8000_0001u32;
    assert_eq!(call(&mut run, &mut io, 1, "sem_take", &[s, timeout as i32]), 0);
    let deadline = io.clock + u64::from(timeout) * u64::from(TICKS_PER_US);
    assert_eq!(get64(&io.memory, tcb(1) + TCB_DEADLINE), deadline, "retain the timeout product's high word");
    io.clock = deadline - 1;
    call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
    waiters(&io, &[1], "one tick before a long deadline");
    assert_eq!(get64(&io.memory, DEADLINE_OUT), deadline);
    io.clock = deadline;
    call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
    completed(&io, 1, 0, "deadline equality expires");
    io.clock = 0;
    for (task, timeout) in [(1, 10), (2, 10), (3, 30)] {
        assert_eq!(call(&mut run, &mut io, task, "sem_take", &[s, timeout]), 0);
    }
    put(&mut io.memory, TASK_COUNT, 3); // valid entries are tasks 0, 1, 2
    io.clock = 10 * u64::from(TICKS_PER_US);
    call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
    completed(&io, 1, 0, "first simultaneous expiry");
    completed(&io, 2, 0, "second simultaneous expiry");
    assert_eq!(io.raised, [YIELD_LINE, YIELD_LINE], "each higher-priority expiration requests yield");
    assert_eq!(get64(&io.memory, DEADLINE_OUT), u64::MAX, "ignore entries beyond TASK_COUNT");
    put(&mut io.memory, TASK_COUNT, TASKS_MAX);
    put(&mut io.memory, tcb(5) + TCB_STATE, BLOCKED); // non-object wait: WAIT_OBJ remains zero
    call(&mut run, &mut io, 0, "kernel_expire", &[DEADLINE_OUT as i32]);
    assert_eq!(get64(&io.memory, DEADLINE_OUT), 30 * u64::from(TICKS_PER_US));
    assert_eq!(get(&io.memory, tcb(5) + TCB_STATE), BLOCKED, "non-object blocked task untouched");
    assert_eq!(call(&mut run, &mut io, 0, "sem_give", &[s]), 1);
    completed(&io, 3, 1, "survivor remains linked after simultaneous expirations");
    call(&mut run, &mut io, 0, "sem_delete", &[s]);
}

#[test]
fn recursive_mutex_and_semaphore_limits_table() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    let m = call(&mut run, &mut io, 0, "mutex_create", &[1]);
    for (scenario, task, op, args, result) in [
        ("owner acquires", 0, "mutex_lock", vec![m, 0], 1),
        ("recursive depth two", 0, "mutex_lock", vec![m, 0], 1),
        ("non-owner cannot unlock", 1, "mutex_unlock", vec![m], 0),
        ("contender waits", 2, "mutex_lock", vec![m, -1], 0),
        ("first unlock retains ownership", 0, "mutex_unlock", vec![m], 1),
    ] {
        assert_eq!(call(&mut run, &mut io, task, op, &args), result, "{scenario}");
        assert!(io.raised.is_empty(), "{scenario}: no ownership transfer yet");
    }
    waiters(&io, &[2], "recursive owner still holds depth one");
    assert_eq!(call(&mut run, &mut io, 0, "mutex_unlock", &[m]), 1);
    completed(&io, 2, 1, "last unlock hands ownership to the waiter");
    assert_eq!(io.raised, [YIELD_LINE]);
    assert_eq!(call(&mut run, &mut io, 0, "mutex_unlock", &[m]), 0, "former owner cannot unlock");
    assert_eq!(call(&mut run, &mut io, 2, "mutex_unlock", &[m]), 1, "new owner can unlock");
    call(&mut run, &mut io, 0, "mutex_delete", &[m]);
    let m = call(&mut run, &mut io, 0, "mutex_create", &[0]);
    assert_eq!(call(&mut run, &mut io, 0, "mutex_lock", &[m, 0]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "mutex_lock", &[m, 0]), 0, "non-recursive zero-timeout relock");
    assert_eq!(get(&io.memory, tcb(0) + TCB_STATE), READY);
    assert_eq!(call(&mut run, &mut io, 0, "mutex_lock", &[m, 10]), 0, "owner relock waits normally");
    waiters(&io, &[0], "non-recursive owner self-wait");
    io.clock = 10 * u64::from(TICKS_PER_US);
    call(&mut run, &mut io, 1, "kernel_expire", &[DEADLINE_OUT as i32]);
    completed(&io, 0, 0, "owner self-wait expires");
    assert_eq!(call(&mut run, &mut io, 0, "mutex_unlock", &[m]), 1, "timeout preserves original ownership");
    call(&mut run, &mut io, 0, "mutex_delete", &[m]);
    assert_eq!(call(&mut run, &mut io, 0, "sem_create", &[2, 3]), 0, "initial exceeds max");
    assert_eq!(call(&mut run, &mut io, 0, "sem_create", &[0, 0]), 0, "zero capacity");
    let s = call(&mut run, &mut io, 0, "sem_create", &[2, 2]);
    for (scenario, op, args, result, count) in [
        ("already at max", "sem_give", vec![s], 0, 2),
        ("take one", "sem_take", vec![s, 0], 1, 1),
        ("give one", "sem_give", vec![s], 1, 2),
        ("still at max", "sem_give", vec![s], 0, 2),
        ("drain first", "sem_take", vec![s, 0], 1, 1),
        ("drain last", "sem_take", vec![s, 0], 1, 0),
        ("empty zero timeout", "sem_take", vec![s, 0], 0, 0),
    ] {
        assert_eq!(call(&mut run, &mut io, 0, op, &args), result, "{scenario}");
        assert_eq!(call(&mut run, &mut io, 0, "sem_count", &[s]), count, "{scenario}: count");
        assert_eq!(get(&io.memory, tcb(0) + TCB_STATE), READY, "{scenario}: no blocking");
    }
    call(&mut run, &mut io, 0, "sem_delete", &[s]);
    let s = call(&mut run, &mut io, 0, "sem_create", &[-1, -1]);
    assert_ne!(s, 0, "unsigned semaphore count");
    assert_eq!(call(&mut run, &mut io, 0, "sem_give", &[s]), 0);
    assert_eq!(call(&mut run, &mut io, 0, "sem_take", &[s, 0]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "sem_give", &[s]), 1);
    assert_eq!(call(&mut run, &mut io, 0, "sem_count", &[s]), -1);
    call(&mut run, &mut io, 0, "sem_delete", &[s]);
}

fn check_heap(io: &KernelPorts<'_>, seed: u32, step: usize) {
    assert_eq!(heap_status(&io.memory), 0, "seed {seed:#x}, operation {step}: heap invariant");
}
struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 17;
        self.0 ^= self.0 << 5;
        self.0
    }
}

#[test]
fn allocation_edges_and_reservations() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    let initial = call(&mut run, &mut io, 0, "heap_free_bytes", &[]);
    call(&mut run, &mut io, 0, "free", &[0]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_failures", &[]), 0, "free(null) does nothing");
    // Exact sizes near class edges and the region's maximum are all served
    // when the heap is empty; there is no raw-copy-size allocation limit.
    for size in [0, 1, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513, initial - 1, initial] {
        let p = call(&mut run, &mut io, 0, "malloc", &[size]);
        assert_ne!(p, 0, "fitting empty-heap request {size}");
        assert_eq!(p & 7, 0, "request {size}: alignment");
        io.memory.bytes_mut(p as u32, size as usize).unwrap().fill(0x35);
        check_heap(&io, 0, size as usize);
        call(&mut run, &mut io, 0, "free", &[p]);
        check_heap(&io, 0, size as usize);
        assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    }
    for (name, args) in [("malloc", vec![initial + 1]), ("malloc", vec![-1]),
        ("calloc_reserve", vec![-1, 2]), ("calloc_reserve", vec![0x4000_0000, 4])] {
        let before = call(&mut run, &mut io, 0, "heap_failures", &[]);
        assert_eq!(call(&mut run, &mut io, 0, name, &args), 0, "{name}{args:?}: failure");
        assert_eq!(call(&mut run, &mut io, 0, "heap_failures", &[]), before + 1, "one failure counted");
        check_heap(&io, 0, 0);
    }
    let zero = call(&mut run, &mut io, 0, "calloc_reserve", &[-1, 0]);
    assert_ne!(zero, 0, "zero product is not overflow");
    call(&mut run, &mut io, 0, "free", &[zero]);
    let a = call(&mut run, &mut io, 0, "realloc_reserve", &[0, 300]);
    assert_ne!(a, 0, "realloc(null, size)");
    io.memory.bytes_mut(a as u32, 300).unwrap().fill(0x71);
    let barrier = call(&mut run, &mut io, 0, "malloc", &[16]);
    assert_ne!(barrier, 0);
    assert_eq!(call(&mut run, &mut io, 0, "realloc_reserve", &[a, initial]), 0,
        "a fitting request fails when the old block must stay allocated during a move");
    assert_eq!(call(&mut run, &mut io, 0, "heap_request_bytes", &[a]), 300);
    let moved = call(&mut run, &mut io, 0, "realloc_reserve", &[a, 700]);
    assert_ne!(moved, 0);
    assert_ne!(moved, a, "live neighbour forces a fresh reservation");
    assert_eq!(call(&mut run, &mut io, 0, "heap_request_bytes", &[a]), 300);
    assert_eq!(call(&mut run, &mut io, 0, "heap_request_bytes", &[moved]), 700);
    assert_eq!(io.memory.bytes(a as u32, 300).unwrap(), &[0x71; 300], "source stays live until the caller frees it");
    check_heap(&io, 0, 0);
    call(&mut run, &mut io, 0, "free", &[a]);
    assert_eq!(call(&mut run, &mut io, 0, "realloc_reserve", &[moved, -1]), 0, "failed realloc retains storage");
    assert_eq!(call(&mut run, &mut io, 0, "heap_request_bytes", &[moved]), 700);
    assert_eq!(call(&mut run, &mut io, 0, "realloc_reserve", &[moved, 257]), moved, "shrink splits and coalesces the tail");
    assert_eq!(call(&mut run, &mut io, 0, "heap_request_bytes", &[moved]), 257);
    check_heap(&io, 0, 0);
    assert_eq!(call(&mut run, &mut io, 0, "realloc_reserve", &[moved, 650]), moved, "grow into the adjacent free tail");
    assert_eq!(call(&mut run, &mut io, 0, "heap_request_bytes", &[moved]), 650);
    check_heap(&io, 0, 0);
    assert_eq!(call(&mut run, &mut io, 0, "realloc_reserve", &[moved, 0]), 0, "realloc(p, 0) frees");
    call(&mut run, &mut io, 0, "free", &[barrier]);
    assert_eq!(call(&mut run, &mut io, 0, "heap_free_bytes", &[]), initial);
    check_heap(&io, 0, 0);
    drop(io);
    assert_eq!(host.reports, [initial + 1, -1, -1].map(|bytes| (0, FAULT_KERNEL_HEAP_EXHAUSTED, bytes)));
}

#[test]
fn heap_check_names_corruption_without_following_bad_links() {
    let mut run = Run::new(compiled_objects());
    let mut host = Host::default();
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    let p = call(&mut run, &mut io, 0, "malloc", &[32]) as u32;
    call(&mut run, &mut io, 0, "free", &[p as i32]);
    let baseline = io.memory.clone();
    // Header offsets are used only to inject corruption. Assertions exercise
    // distinct checker guarantees, including a link outside mapped memory.
    for (scenario, at, value, code) in [
        ("control magic", HEAP, 1, 1),
        ("zeroed magic is not a fresh heap", HEAP, 0, 1),
        ("initialization cookie", HEAP + 20, 0, 1),
        ("physical size", p - 16, 1, 2),
        ("physical back-link", p - 12, p, 3),
        ("free accounting", HEAP + 8, 0, 6),
        ("bitmap high bit", HEAP + 12, 0x8000_0000, 7),
        ("out of region list link", p, 0xffff_fff8, 8),
        ("free back-link", p + 4, p, 9),
        ("duplicate free membership", p, p - 16, 10),
    ] {
        io.memory = baseline.clone();
        put(&mut io.memory, at, value);
        let before = io.memory.bytes(HEAP, HEAP_BYTES as usize).unwrap().to_vec();
        // The readers use checked slices: following the corrupt link would
        // panic instead of returning the expected failure code.
        assert_eq!(heap_status(&io.memory), code, "{scenario}: first failed invariant");
        assert_eq!(io.memory.bytes(HEAP, HEAP_BYTES as usize).unwrap(), before, "{scenario}: checker is read-only");
    }
    io.memory = baseline;
    let a = call(&mut run, &mut io, 0, "malloc", &[32]);
    let b = call(&mut run, &mut io, 0, "malloc", &[32]);
    put(&mut io.memory, b as u32 - 8, u32::MAX);
    assert_eq!(heap_status(&io.memory), 5, "allocated request exceeds payload");
    put(&mut io.memory, b as u32 - 8, 32);
    call(&mut run, &mut io, 0, "free", &[a]);
    let header = b as u32 - 16;
    let size = get(&io.memory, header);
    put(&mut io.memory, header, size | 1);
    assert_eq!(heap_status(&io.memory), 4, "adjacent free physical blocks");
}

#[test]
fn critical_sections_count_nesting_and_only_report_bad_tokens() {
    for outer in [0, 3, 5] {
        for bad_token_at in [None, Some(0), Some(1)] {
            let mut run = Run::new(compiled_objects());
            let slot = |name| run.module.vars().iter().find(|v| run.module.string(v.name) == name).unwrap().offset as usize;
            let (depth, saved, exit) = (slot("crit_depth"), slot("crit_outer"), slot("osi_crit_level"));
            let mut host = Host { accept_reports: true, ..Host::default() };
            let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
            let before = io.memory.clone();
            let masked = outer.max(CRIT_MASK);
            let token0 = run.call("osi_wifi_int_disable", &[0, outer as i32, 0x4000_1010, 0], &mut io);
            assert_eq!(token0, outer as i32);
            assert_eq!([run.state[depth], run.state[saved], run.state[exit]], [1, outer, masked]);
            let token1 = run.call("osi_wifi_int_disable", &[0, masked as i32, 0x4000_1020, 0], &mut io);
            assert_eq!(token1, masked as i32);
            assert_eq!([run.state[depth], run.state[saved], run.state[exit]], [2, outer, masked]);
            for (i, token) in [token1, token0].into_iter().enumerate() {
                let ra = 0x4000_1030 + i as i32 * 16;
                let token = if bad_token_at == Some(i) { token ^ 15 } else { token };
                assert_eq!(run.call("osi_wifi_int_restore", &[0, token, masked as i32, ra, 0], &mut io), 0);
                assert_eq!([run.state[depth], run.state[saved], run.state[exit]],
                    [1 - i as u32, outer, if i == 0 { masked } else { outer }],
                    "a token mismatch does not change nesting or level restoration");
            }
            for r in regions() {
                assert_eq!(io.memory.bytes(r.base, r.len as usize), before.bytes(r.base, r.len as usize),
                    "sections and mismatch reports do not change device or object memory");
            }
            drop(io);
            let expected: Vec<_> = bad_token_at.into_iter().map(|i|
                (0, FAULT_KERNEL_CRIT_MISMATCH, 0x4000_1030 + i as i32 * 16)).collect();
            assert_eq!(host.reports, expected, "only the mismatched restore reports its caller RA");
        }
        let mut run = Run::new(compiled_objects());
        let slot = |name| run.module.vars().iter().find(|v| run.module.string(v.name) == name).unwrap().offset as usize;
        let (depth, saved, exit) = (slot("crit_depth"), slot("crit_outer"), slot("osi_crit_level"));
        let mut host = Host { accept_reports: true, ..Host::default() };
        let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
        run.state[saved] = outer; // retain the level saved by an earlier, completed section
        assert_eq!(run.call("osi_wifi_int_restore", &[0, -1, outer as i32, 0x4000_1050, 0], &mut io), 0);
        assert_eq!([run.state[depth], run.state[saved], run.state[exit]], [0, outer, outer],
            "an unmatched restore keeps its entry level and does not underflow");
        drop(io);
        assert_eq!(host.reports, [(0, FAULT_KERNEL_CRIT_MISMATCH, 0x4000_1050)]);
    }
}

#[test]
fn critical_spans_track_outer_task_sections_with_unsigned_cycle_stamps() {
    let compiled = compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES, &kernel_consts(&layout())).unwrap();
    let mut run = Run::new(&compiled);
    // The image writes the largest gate bound here; 100 exercises the reporting path.
    let threshold = run.module.vars().iter().find(|v| run.module.string(v.name) == "crit_span_report").unwrap().offset;
    assert_eq!(run.state[threshold as usize], u32::MAX, "an unset threshold never reports");
    run.state[threshold as usize] = 100;
    let slot = |name| run.module.vars().iter().find(|v| run.module.string(v.name) == name).unwrap().offset as usize;
    let (start_slot, start_ra, max_span, max_ra) =
        (slot("crit_start"), slot("crit_start_ra"), slot("crit_max_span"), slot("crit_max_ra"));
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    // These are explicit CCOUNT values supplied as the thunk's hidden argument.
    // KernelPorts::clock is independent and must not affect the cycle marks.
    io.clock = 12345;
    for (index, (start, end, ra, expected_span, expected_ra)) in [
        (10u32, 100u32, 0x4000_1010u32, 90u32, 0x4000_1010u32),
        (200, 300, 0x4000_1020, 100, 0x4000_1020), // equality to threshold is quiet
        (400, 501, 0x4000_1030, 101, 0x4000_1030),
        (700, 750, 0x4000_1040, 101, 0x4000_1030), // shorter: keep both maximum words
        (800, 901, 0x4000_1050, 101, 0x4000_1030), // equal maximum is also quiet
        (u32::MAX - 49, 100, 0x4000_1060, 150, 0x4000_1060),
        (0, 0x8000_0010, 0x4000_1070, 0x8000_0010, 0x4000_1070),
    ].into_iter().enumerate() {
        assert_eq!(run.call("osi_wifi_int_disable", &[0, 0, ra as i32, start as i32], &mut io), 0);
        assert_eq!((run.state[start_slot], run.state[start_ra]), (start, ra));
        if index == 0 {
            assert_eq!(run.call("osi_wifi_int_disable", &[0, 3, 0x4000_9990, 40], &mut io), 3);
            assert_eq!(run.call("osi_wifi_int_restore", &[0, 3, 3, 0x4000_9994, 80], &mut io), 0);
            assert_eq!((run.state[start_slot], run.state[start_ra]), (start, ra), "inner stamps do not replace the opening site");
            assert_eq!((run.state[max_span], run.state[max_ra]), (0, 0), "only the outer restore measures");
        }
        assert_eq!(run.call("osi_wifi_int_restore", &[0, 0, 3, 0x4000_9998, end as i32], &mut io), 0);
        assert_eq!((run.state[max_span], run.state[max_ra]), (expected_span, expected_ra));
    }
    let saved = [run.state[start_slot], run.state[start_ra], run.state[max_span], run.state[max_ra]];
    for level in [3, 5] {
        assert_eq!(run.call("osi_wifi_int_disable", &[0, level, 0x4000_2000, 100], &mut io), level);
        assert_eq!(run.call("osi_wifi_int_restore", &[0, level, level, 0x4000_2004, 99], &mut io), 0);
        assert_eq!([run.state[start_slot], run.state[start_ra], run.state[max_span], run.state[max_ra]], saved,
            "sections entered masked record no task span, even if their unsigned span would be larger");
    }
    drop(io);
    assert_eq!(host.reports, [101u32, 150, 0x8000_0010].map(|span|
        (12345, FAULT_KERNEL_CRIT_SPAN, span as i32)), "report each above-threshold new maximum, preserving bit 31");
}

#[test]
fn phy_sequence_owns_clocks_calibration_and_the_mutex() {
    // Test-only exports let the future power-domain owner share the real
    // common-clock helpers without exposing extra production gates.
    let source = format!("{OBJECTS}\n\
        let test_common_hold = export(fn() {{ clk_common_hold(); return 0 }})\n\
        let test_common_release = export(fn() {{ clk_common_release(); return 0 }})");
    let compiled = compile_kernel(&source, &chip::profile(), &RADIO_KERNEL_LINES, &kernel_consts(&layout())).unwrap();
    let mut run = Run::new(&compiled);
    let [modem, calibrated, dirty, stored, common, last_low, last_high] =
        ["phy_modem", "phy_calibrated", "phy_cal_dirty", "phy_regs_stored",
            "clk_common_count", "phy_track_last_low", "phy_track_last_high"].map(|name|
                run.module.vars().iter().find(|v| run.module.string(v.name) == name).unwrap().offset as usize);
    let persistent: Vec<_> = run.module.vars().iter().filter(|v| !run.module.string(v.name).contains('#'))
        .map(|v| v.offset as usize).collect();
    let no_owner = |run: &mut Run<'_>, io: &mut KernelPorts<'_>, task: u32| {
        put(&mut io.memory, CURRENT, tcb(task));
        for (name, args, result) in [
            ("phy_enter", [PHY_OP_ENABLE as i32, 0], PHY_SKIP as i32),
            ("phy_enter", [PHY_OP_DISABLE as i32, 0], PHY_SKIP as i32),
            ("phy_init_done", [PHY_FIRST as i32, -9], 0),
            ("phy_leave", [PHY_OP_DISABLE as i32, PHY_LAST as i32], 0),
        ] {
            let before = io.memory.clone();
            let state: Vec<_> = persistent.iter().map(|&s| run.state[s]).collect();
            assert_eq!(run.call(name, &args, io), result, "{name}: non-owner returns SKIP/zero");
            assert_eq!(format!("{:?}", io.memory), format!("{before:?}"), "{name}: non-owner touches no memory, even with a null out pointer");
            assert_eq!(persistent.iter().map(|&s| run.state[s]).collect::<Vec<_>>(), state,
                "{name}: non-owner changes no persistent state");
        }
    };
    let mut host = Host { accept_reports: true, ..Host::default() };
    let mut io = KernelPorts::new(memory(), RADIO_KERNEL_LINES, &mut host);
    const OTHER_CLOCKS: u32 = 0x8000_4010;
    let out = BUFFERS;
    let enable = PHY_OP_ENABLE as i32;
    let disable = PHY_OP_DISABLE as i32;
    put(&mut io.memory, SYSCON_WIFI_CLK_EN, OTHER_CLOCKS);
    no_owner(&mut run, &mut io, 0);

    // Two owners share common clocks; neither a partial nor an extra release
    // may clear unrelated bits or underflow the count.
    for _ in 0..2 { call(&mut run, &mut io, 0, "test_common_hold", &[]); }
    call(&mut run, &mut io, 0, "test_common_release", &[]);
    assert_eq!((run.state[common], get(&io.memory, SYSCON_WIFI_CLK_EN)), (1, OTHER_CLOCKS | CLK_COMMON),
        "one common-clock owner remains after two holds and one release");
    call(&mut run, &mut io, 0, "test_common_release", &[]);
    call(&mut run, &mut io, 0, "test_common_release", &[]);
    assert_eq!((run.state[common], get(&io.memory, SYSCON_WIFI_CLK_EN)), (0, OTHER_CLOCKS),
        "last release clears only common clocks; an unmatched release is inert");

    let heap_before = io.memory.bytes(HEAP, HEAP_BYTES as usize).unwrap().to_vec();
    assert_eq!(call(&mut run, &mut io, 0, "phy_lock", &[]), 1, "locks the fixed zeroed mutex");
    assert_eq!(io.memory.bytes(HEAP, HEAP_BYTES as usize).unwrap(), heap_before,
        "PHY lock never allocates or initializes the heap");
    let m = PHY_MUTEX;
    io.memory.bytes_mut(PHY_CAL, 1904).unwrap().fill(0xa5);
    assert_eq!(call(&mut run, &mut io, 0, "phy_enter", &[enable, out as i32]), PHY_FIRST as i32,
        "uncalibrated first start selects the full initialization sequence");
    assert_eq!([get(&io.memory, out), get(&io.memory, out + 4), get(&io.memory, out + 8), get(&io.memory, out + 12)],
        [PHY_INIT, PHY_CAL, PHY_RF_CAL_FULL, PHY_REGS], "out block provides all four glue operands");
    assert_eq!(io.memory.bytes(PHY_CAL + 4, 6).unwrap(), &[0x02, 0x34, 0x89, 0xab, 0xcd, 0xff],
        "calibration receives the eFuse STA MAC");
    assert!(io.memory.bytes(PHY_CAL, 4).unwrap().iter().chain(io.memory.bytes(PHY_CAL + 10, 1894).unwrap()).all(|&b| b == 0xa5),
        "MAC preparation changes neither calibration version nor opaque data");
    assert_eq!(get(&io.memory, SYSCON_WIFI_CLK_EN), OTHER_CLOCKS | CLK_COMMON | CLK_BT_BASEBAND | CLK_RNG,
        "common and module clocks are enabled before the first libphy call");
    assert_eq!(run.state[common], 1, "first enable owns exactly one common-clock reference");
    assert!(io.memory.bytes_mut(EFUSE_MAC_0, 4).is_err(), "eFuse remains read-only");

    let period = u64::from(PHY_TRACK_PLL_PERIOD_US);
    io.clock = (period - 1) * u64::from(TICKS_PER_US);
    assert_eq!(call(&mut run, &mut io, 0, "phy_init_done", &[PHY_FIRST as i32, 0]), 0,
        "stamp starts at zero and a shorter interval needs no track");
    assert_eq!([run.state[calibrated], run.state[dirty], run.state[modem]], [1, 1, PHY_MODEM_WIFI],
        "first completion records calibration, pending storage and Wi-Fi ownership");
    assert_eq!(get(&io.memory, SYSCON_WIFI_CLK_EN), OTHER_CLOCKS | CLK_COMMON | CLK_RNG,
        "completion clears only baseband; common and RNG clocks stay on");
    let mut last = 0u64;
    for (us, due) in [(period, 0), (period + 1, 1), (2 * period + 1, 0), (2 * period + 2, 1),
        ((1u64 << 32) - 50, 1), ((1u64 << 32) + period - 50, 0),
        ((1u64 << 32) + period - 49, 1), ((3u64 << 32) + 37, 1)] {
        io.clock = us * u64::from(TICKS_PER_US);
        assert_eq!(call(&mut run, &mut io, 0, "phy_init_done", &[PHY_ON as i32, 0]), due,
            "PLL strict period and full-width subtraction at {us} us");
        if due != 0 { last = us; }
        assert_eq!(u64::from(run.state[last_low]) | (u64::from(run.state[last_high]) << 32), last,
            "only a due track updates both stamp limbs");
    }
    let before = io.memory.clone();
    assert_eq!(call(&mut run, &mut io, 0, "phy_init_done", &[PHY_SKIP as i32, -9]), 0, "SKIP does no completion work");
    assert_eq!(format!("{:?}", io.memory), format!("{before:?}"), "SKIP does not read the clock into heap scratch or change registers");
    call(&mut run, &mut io, 0, "phy_leave", &[enable, PHY_FIRST as i32]);
    assert_eq!(get(&io.memory, m + 4), 0, "leave unlocks the PHY mutex");

    call(&mut run, &mut io, 0, "phy_lock", &[]);
    let clocks = get(&io.memory, SYSCON_WIFI_CLK_EN);
    assert_eq!(call(&mut run, &mut io, 0, "phy_enter", &[enable, out as i32]), PHY_ON as i32, "second enable is already on");
    assert_eq!(get(&io.memory, SYSCON_WIFI_CLK_EN), clocks, "already-on enter preserves every clock bit");
    assert_eq!(run.state[common], 1, "already-on enter takes no extra common reference");

    // The host changes CURRENT exactly as R3 would on a task switch. Task 1
    // enqueues; leave publishes ownership before making it READY.
    assert_eq!(call(&mut run, &mut io, 1, "phy_lock", &[]), 0, "second task waits for the PHY mutex");
    waiters(&io, &[1], "PHY mutex contention");
    no_owner(&mut run, &mut io, 1);
    call(&mut run, &mut io, 0, "phy_leave", &[enable, PHY_ON as i32]);
    completed(&io, 1, 1, "PHY mutex handoff");
    assert_eq!(get(&io.memory, m + 4), tcb(1), "handoff owns the mutex before the waiter resumes");
    no_owner(&mut run, &mut io, 0);

    // A second modem is a fixture state: this glue exposes only Wi-Fi.
    run.state[modem] = PHY_MODEM_WIFI | 2;
    assert_eq!(call(&mut run, &mut io, 1, "phy_enter", &[disable, out as i32]), PHY_NOT_LAST as i32, "another modem prevents RF shutdown");
    assert_eq!(run.state[modem], 2, "disable removes only the Wi-Fi modem bit");
    call(&mut run, &mut io, 1, "phy_leave", &[disable, PHY_NOT_LAST as i32]);
    assert_eq!((get(&io.memory, SYSCON_WIFI_CLK_EN), run.state[stored], run.state[common]), (clocks, 0, 1),
        "non-last disable leaves clocks, backup status and common ownership alone");
    run.state[modem] = PHY_MODEM_WIFI; // fixture now has Wi-Fi as its only modem
    call(&mut run, &mut io, 0, "phy_lock", &[]);
    assert_eq!(call(&mut run, &mut io, 0, "phy_enter", &[disable, out as i32]), PHY_LAST as i32, "Wi-Fi is the last modem");
    assert_eq!((run.state[modem], get(&io.memory, SYSCON_WIFI_CLK_EN)), (0, clocks), "last enter leaves clocks on for libphy shutdown");
    call(&mut run, &mut io, 0, "phy_leave", &[disable, PHY_LAST as i32]);
    assert_eq!((run.state[stored], run.state[common], get(&io.memory, SYSCON_WIFI_CLK_EN)), (1, 0, OTHER_CLOCKS | CLK_RNG),
        "last leave records the backup, releases common clocks and retains RNG");

    // Exercise restored wake, wake without a backup, and both nonzero first
    // calibration results. Only fixture state selects those starting states.
    for (step, result) in [(PHY_WAKE_RESTORE, 0), (PHY_WAKE, 0), (PHY_FIRST, 1), (PHY_FIRST, -7)] {
        if step == PHY_WAKE { run.state[stored] = 0; }
        if step == PHY_FIRST { run.state[calibrated] = 0; run.state[dirty] = 0; }
        call(&mut run, &mut io, 0, "phy_lock", &[]);
        assert_eq!(call(&mut run, &mut io, 0, "phy_enter", &[enable, out as i32]), step as i32, "select PHY path {step}");
        assert_eq!(get(&io.memory, SYSCON_WIFI_CLK_EN), OTHER_CLOCKS | CLK_COMMON | CLK_BT_BASEBAND | CLK_RNG,
            "PHY path {step} re-enables module clocks before libphy");
        assert_eq!(call(&mut run, &mut io, 0, "phy_init_done", &[step as i32, result]), 0, "same clock needs no second track");
        assert_eq!((run.state[calibrated], run.state[dirty]), (1, 1), "PHY result {result} keeps calibration state");
        assert_eq!(get(&io.memory, SYSCON_WIFI_CLK_EN), OTHER_CLOCKS | CLK_COMMON | CLK_RNG, "PHY path {step} clears bit 11 only");
        call(&mut run, &mut io, 0, "phy_leave", &[enable, step as i32]);
        call(&mut run, &mut io, 0, "phy_lock", &[]);
        assert_eq!(call(&mut run, &mut io, 0, "phy_enter", &[disable, out as i32]), PHY_LAST as i32);
        call(&mut run, &mut io, 0, "phy_leave", &[disable, PHY_LAST as i32]);
    }
    assert_eq!((get(&io.memory, EFUSE_MAC_0), get(&io.memory, EFUSE_MAC_1)), (0x89ab_cdff, 0xbeef_0234), "PHY never writes eFuse");
    no_owner(&mut run, &mut io, 0);
    let report_tick = io.clock;
    drop(io);
    assert_eq!(host.reports, [(report_tick, FAULT_RADIO_PHY_INIT_FAILED, -7)],
        "only an unexpected init result reports code 100; checksum result 1 and SKIP are quiet");
}

#[test]
fn build_all_objects_and_report_masked_instruction_bounds() {
    let run = Run::new(compiled_objects());
    let options = KernelOptions { base: 0x4008_0000, state: 0x18000, t0: 0x19000, reports: Some(REPORTS), stop_pending: Some(STOP_PENDING), regions: regions() };
    let code = build_kernel(&run.module, &options).expect("all object exports emit as kernel code");
    let mut names: Vec<_> = code.entries.iter().map(|entry| entry.name.as_str()).collect();
    let mut expected = EXPORTS.to_vec();
    names.sort_unstable();
    expected.sort_unstable();
    assert_eq!(names, expected, "every requested export, without queue_remove");
    let persistent: Vec<_> = run.module.vars().iter().map(|v| run.module.string(v.name))
        .filter(|name| !name.contains('#')).collect();
    assert_eq!(persistent, ["task_exit_link", "crit_depth", "crit_outer", "osi_crit_level", "crit_start", "crit_start_ra",
        "crit_max_span", "crit_max_ra", "crit_span_report", "osi_time_high", "osi_rng_last",
        "phy_init_data", "phy_modem", "phy_calibrated", "phy_cal_dirty", "phy_regs_stored",
        "clk_common_count", "phy_track_last_low", "phy_track_last_high", "dropped_prints",
        "event_head", "event_tail"],
        "persistent state rebuilt on radio restart");
    for (name, address) in [("crit_depth", code.crit_depth), ("osi_crit_level", code.osi_crit_level),
        ("crit_max_span", code.crit_max_span), ("crit_max_ra", code.crit_max_ra),
        ("crit_span_report", code.crit_span_report), ("task_exit_link", code.task_exit_link),
        ("phy_init_data", code.phy_init_data)] {
        let var = run.module.vars().iter().find(|v| run.module.string(v.name) == name).unwrap();
        assert_eq!(address, Some(options.state + 4 * var.offset), "{name}: absolute state slot");
    }
    eprintln!("max_insns: heap={HEAP_BYTES}, max_tasks={TASKS_MAX}, isr_capacity={PACK_ISR_LINES}, regions={}", options.regions.len());
    eprintln!("| Export | max_insns |\n| --- | ---: |");
    for entry in &code.entries {
        assert!(entry.max_insns > 0, "{}: nonzero emitted instruction bound", entry.name);
        eprintln!("| {} | {} |", entry.name, entry.max_insns);
    }

    // Keep state and the clock origin outside the larger heap allocation.
    let planned = KernelLayout { heap_base: 0x4_0000, heap_size: PLANNED_HEAP_BYTES, timer_reserve: 64,
        task_count: TASKS + PLANNED_MAX_TASKS * 4, max_tasks: PLANNED_MAX_TASKS,
        ..layout() };
    let mut options = KernelOptions { regions: vec![
        KernelRegion { base: planned.heap_base, len: planned.heap_size, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: TCBS, len: planned.max_tasks * TCB_SIZE as u32, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: CURRENT, len: CURSOR_OFFSET + 4, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: TASKS, len: (planned.max_tasks + 1) * 4, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: BUFFERS, len: 2048, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: ISRS, len: ISR_COUNT - ISRS + 4, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: PHY_INIT, len: 128, writable: false, kind: RegionKind::Pack },
        KernelRegion { base: PHY_CAL, len: 1904, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: PHY_REGS, len: 84, writable: true, kind: RegionKind::Kernel },
        KernelRegion { base: PHY_MUTEX, len: radio::kernel::MUTEX_BYTES, writable: true, kind: RegionKind::Kernel },
    ], ..options };
    options.regions.extend(device_regions());
    use fab_elec_mcu_emit_s3::radio::kernel::gates::{gates, EntryRegs, EXIT_LEVEL_EXPORTS};
    let mut emit_for_heap = |heap_size, timer_reserve| {
        let layout = KernelLayout { heap_size, timer_reserve, ..planned };
        let bindings = kernel_consts(&layout);
        let sift_cap = bindings.iter().find(|(name, _)| *name == "TIMER_SIFT_CAP").unwrap().1;
        let compiled = compile_kernel(OBJECTS, &chip::profile(), &RADIO_KERNEL_LINES, &bindings)
            .unwrap_or_else(|errors| panic!("objects.splash with {heap_size} heap bytes: {errors:?}"));
        let module = load_kernel(&compiled.module, &chip::profile(), &RADIO_KERNEL_LINES).unwrap();
        options.regions.iter_mut().find(|r| r.base == layout.heap_base).unwrap().len = heap_size;
        let code = build_kernel(&module, &options)
            .unwrap_or_else(|err| panic!("kernel with {heap_size} heap bytes: {err:?}"));
        let routines = gates((code.base + code.bytes.len() as u32 + 3) & !3, &code,
            EntryRegs { state: options.state, shared: 0x1a000 });
        (sift_cap, code.entries, routines.gates)
    };
    let (_, entries, mut planned_gates) = emit_for_heap(PLANNED_HEAP_BYTES, planned.timer_reserve);
    eprintln!("planned max_insns: heap={PLANNED_HEAP_BYTES}, max_tasks={PLANNED_MAX_TASKS}, isr_capacity={PACK_ISR_LINES}");
    eprintln!("| Export | Entry max_insns | Gate max_insns | Port calls | Port insns | Walk insns | Walk % of gate |");
    eprintln!("| --- | ---: | ---: | ---: | ---: | ---: | ---: |");
    assert_eq!(entries.len(), planned_gates.len(), "every planned export has a gate to report");
    for (entry, gate) in entries.iter().zip(&planned_gates) {
        assert_eq!(entry.name, gate.name);
        assert!(entry.max_insns > 0, "{}: planned instruction bound", entry.name);
        let (mut calls, mut insns, mut walk) = (0u64, 0u64, 0u64);
        for charge in &gate.port_charges {
            assert!(charge.calls > 0, "{} {}: a listed port has calls", gate.name, charge.name());
            assert!(charge.walk <= charge.insns, "{} {}: walk fits in port charge", gate.name, charge.name());
            calls += charge.calls;
            insns += charge.insns;
            walk += charge.walk;
        }
        assert!(insns <= gate.max_insns, "{}: port charges fit in gate bound", gate.name);
        let share = 100.0 * walk as f64 / gate.max_insns as f64;
        eprintln!("| {} | {} | {} | {calls} | {insns} | {walk} | {share:.1}% |", gate.name, entry.max_insns, gate.max_insns);
    }
    planned_gates.sort_by_key(|gate| gate.max_insns);
    eprintln!("| Planned gate | Port | Calls | Insns | Walk insns |\n| --- | --- | ---: | ---: | ---: |");
    for gate in planned_gates.iter().rev().take(5) {
        for charge in &gate.port_charges {
            eprintln!("| {} | {} | {} | {} | {} |", gate.name, charge.name(), charge.calls, charge.insns, charge.walk);
        }
    }
    let largest = planned_gates.last().unwrap();
    eprintln!("planned largest masked gate: {} = {} instructions", largest.name, largest.max_insns);
    // With a stop word port 78 charges all emitted arms: 15 through publication,
    // 5 in the loss arm, 4 comparisons/offset, RET, 5 to stop, CALLX0 and ILL.
    // The ring-only form charged 15+5+1+1=22. The production form charges 32.
    // Port 86 charges its five stop instructions plus CALLX0 and ILL.
    let failure = planned_gates.iter().find(|g| g.name == "radio_start_failed").unwrap();
    let failure_entry = entries.iter().find(|e| e.name == failure.name).unwrap();
    assert_eq!(failure.max_insns, 11 + 2 + failure_entry.max_insns, "one-argument CALL0 wrapper");
    for (port, cost) in [(fab_elec_mcu_rt::ports::KERNEL_REPORT, 32), (fab_elec_mcu_rt::ports::RADIO_STOP, 7)] {
        let charge = failure.port_charges.iter().find(|p| p.port == port).unwrap();
        assert_eq!((charge.calls, charge.insns), (1, cost));
    }
    for name in ["malloc", "calloc_reserve", "realloc_reserve", "task_create", "timer_setfn", "timer_arm",
        "timer_arm_us", "timer_disarm", "timer_done", "timer_due", "kernel_expire"] {
        let gate = planned_gates.iter().find(|g| g.name == name).unwrap();
        eprintln!("planned {name} gate max_insns: {}", gate.max_insns);
    }
    for name in ["phy_lock", "phy_enter", "phy_init_done", "phy_leave",
        "puts", "net80211_printf", "pp_printf", "phy_printf"] {
        let gate = planned_gates.iter().find(|g| g.name == name).unwrap();
        assert_eq!(gate.blocks, name == "phy_lock", "{name}: only lock may block");
        assert!(!EXIT_LEVEL_EXPORTS.contains(&name), "{name}: ordinary masking gate");
        eprintln!("planned {name} gate max_insns: {}", gate.max_insns);
    }

    // TLSF level counts 8, 10 and 12 are all Xtensa branch immediates, so a gate that does not
    // depend on the heap size emits exactly the same bound at these three sizes.
    let [(cap4, bounds4), (cap16, bounds16), (cap64, bounds64)] = [4, 16, 64].map(|kib| {
        // Use the same reserve at all three sizes. Eight fits the 4 KiB fixture.
        let (cap, _, gates) = emit_for_heap(kib * 1024, 8);
        let bounds: std::collections::BTreeMap<_, _> = gates.into_iter().map(|g| (g.name, g.max_insns)).collect();
        assert_eq!(bounds.len(), EXPORTS.len(), "export gate count at {kib} KiB");
        (cap, bounds)
    });
    let levels_low = (cap16 - cap4) as u64;
    let levels_high = (cap64 - cap16) as u64;
    assert_eq!((levels_low, levels_high), (2, 2), "added sift levels for 56-byte timer blocks");
    const SIFTING: [&str; 5] = ["timer_arm", "timer_arm_us", "timer_disarm", "timer_done", "timer_due"];
    // The least any sifting gate grows per added level.
    let level_cost = SIFTING.iter().map(|&name| (bounds16[name] - bounds4[name]) / levels_low).min().unwrap();
    let mut mismatches = Vec::new();
    for &name in EXPORTS {
        let (bound4, bound16, bound64) = (bounds4[name], bounds16[name], bounds64[name]);
        let fits = if SIFTING.contains(&name) {
            // Sifting keeps the growth per added level steady. A walk over the nodes would make
            // the upper span's slope about four times the lower one (878 more nodes against 219).
            bound4 < bound16 && bound16 < bound64
                && (bound64 - bound16) * levels_low <= 2 * (bound16 - bound4) * levels_high
        } else if name == "timer_setfn" {
            // setfn never sifts: a second setfn is ignored and a new timer is not armed. Its slot
            // directory bound (2, 5, 19 words) need not be a branch immediate, so it may move by
            // an instruction, never by a sift level.
            bound4.max(bound16).max(bound64) - bound4.min(bound16).min(bound64) < level_cost
        } else {
            bound4 == bound16 && bound16 == bound64
        };
        if !fits { mismatches.push(format!("{name}: {bound4}, {bound16}, {bound64}")); }
    }
    assert!(mismatches.is_empty(), "expected sifting timer gates to grow by sift levels, not by timer count, \
        setfn to stay within one level's cost ({level_cost}), and equal bounds for every other gate. \
        Mismatches at 4, 16, 64 KiB (sift caps {cap4}, {cap16}, {cap64}):\n{}",
        mismatches.join("\n"));
}

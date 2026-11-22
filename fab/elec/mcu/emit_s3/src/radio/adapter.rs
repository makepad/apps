//! ESP32-S3 Wi-Fi OS interface. This table is separate from island imports;
//! R4c-3 supplies a second `thunk::exports` set, preserving the island ABI hash.

use fab_elec_mcu_emit32::xtensa::{assemble, cc, ccz, enc, reg::*, Asm};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    /// The table word itself, not an address of a function.
    Data(u32),
    /// Kernel export reached through an R3 windowed thunk.
    Gate(&'static str),
    /// Kernel export also returning its high word in a3.
    Gate64(&'static str),
    /// Absolute windowed entry of an island glue object, past its literals.
    Glue(&'static str),
    /// Unmasked windowed byte-fill loop calling a one-word masked export.
    Chunked(&'static str),
    /// Reserve through masked thunks, then zero or copy at the caller's interrupt level.
    Calloc,
    Zalloc,
    Realloc,
    /// Windowed stub returning this value in a2.
    Const(i32),
    /// Named ILL stub: the pack must never call this field.
    Fault,
    /// Named ILL stub until the designated step supplies this field.
    Pending(&'static str),
}

impl Entry {
    /// Thunks needed by this row, in call order.
    pub fn kernel_exports(&self) -> &[&'static str] {
        match self {
            Self::Gate(name) | Self::Gate64(name) | Self::Chunked(name) => std::slice::from_ref(name),
            Self::Calloc => &["calloc_reserve"],
            Self::Zalloc => &["malloc"],
            Self::Realloc => &["heap_request_bytes", "realloc_reserve", "free"],
            Self::Data(_) | Self::Glue(_) | Self::Const(_) | Self::Fault | Self::Pending(_) => &[],
        }
    }
}

pub const OSI_VERSION: u32 = 9;
pub const OSI_MAGIC: u32 = 0xDEAD_BEAF;
/// Assumed IDF default configMAX_PRIORITIES (FreeRTOSConfig is absent from
/// the pinned tree); verify on the board in R9.
pub const BLOB_MAX_PRIORITY: i32 = 25;
/// Unverified: IDF's value from memory. Its source, components/nvs_flash/include/nvs.h, is not in
/// the pinned trees. Any nonzero value is a failure to the caller.
pub const ESP_ERR_NVS_NOT_INITIALIZED: i32 = 0x1101;
/// Unverified, as ESP_ERR_NVS_NOT_INITIALIZED.
pub const ESP_ERR_NVS_INVALID_HANDLE: i32 = 0x1107;

/// S3 order: the pinned header with the ESP32-S3 conditionals evaluated (120 of its 130 names).
/// Meanings and signatures: IDF v6.1.1 (Apache-2.0),
/// components/esp_wifi/include/esp_private/wifi_os_adapter.h and
/// components/esp_wifi/esp32s3/esp_adapter.c. Second reading: esp-radio
/// 4059aa584b579ac2e78903c627b6daa9d5d2d90c (MIT OR Apache-2.0),
/// esp-radio/src/wifi/os_adapter/mod.rs (reference only).
/// Pending entries deliberately fault until their named step supplies them.
pub const OSI_FIELDS: [(&str, Entry); 120] = {
    use Entry::*;
    [
        ("_version", Data(OSI_VERSION)), // IDF ESP_WIFI_OS_ADAPTER_VERSION.
        ("_env_is_chip", Const(1)), // IDF: true on silicon.
        // IDF v6.1.1 components/esp_wifi/esp32s3/esp_adapter.c:638;
        // esp-radio esp-radio/src/wifi/os_adapter/esp32s3.rs:17-30 (CPU 0).
        ("_set_intr", Gate("osi_set_intr")),
        ("_clear_intr", Const(0)), // IDF S3 clear_intr_wrapper is empty.
        // IDF v6.1.1 components/esp_wifi/esp32s3/esp_adapter.c:640;
        // esp-radio esp-radio/src/wifi/os_adapter/esp32s3.rs:47-72 (shared MAC/PWR handler).
        ("_set_isr", Gate("osi_set_isr")),
        ("_ints_on", Gate("osi_ints_on")),
        ("_ints_off", Gate("osi_ints_off")),
        ("_is_from_isr", Fault),
        ("_spin_lock_create", Gate("osi_spin_lock_create")), // Brief/esp-radio: semaphore (1, 1).
        ("_spin_lock_delete", Gate("sem_delete")), // esp-radio: sem_delete; IDF frees its own lock object.
        // IDF v6.1.1 components/esp_wifi/esp32s3/esp_adapter.c:646-647:
        // wrappers in unpinned esp_coex; token handling remains unverified.
        // esp-radio esp-radio/src/wifi/os_adapter/mod.rs:200-226
        // (MIT OR Apache-2.0): one global lock, ignoring the opaque mux.
        ("_wifi_int_disable", Gate("osi_wifi_int_disable")),
        ("_wifi_int_restore", Gate("osi_wifi_int_restore")),
        ("_task_yield_from_isr", Const(0)), // Assigned no-op; scheduler picks after ISR dispatch.
        ("_semphr_create", Gate("sem_create")), // IDF: (max, initial), handle or NULL.
        ("_semphr_delete", Gate("sem_delete")), // IDF: (handle), void.
        ("_semphr_take", Gate("osi_semphr_take")), // IDF: ticks, pdTRUE/pdFALSE.
        ("_semphr_give", Gate("sem_give")), // IDF: (handle), pdTRUE/pdFALSE.
        ("_wifi_thread_semphr_get", Gate("osi_wifi_thread_semphr_get")), // IDF/esp-radio: per task.
        ("_mutex_create", Gate("osi_mutex_create")), // IDF/esp-radio: nonrecursive.
        ("_recursive_mutex_create", Gate("osi_recursive_mutex_create")), // IDF/esp-radio: recursive.
        ("_mutex_delete", Gate("mutex_delete")), // IDF: (handle), void.
        ("_mutex_lock", Gate("osi_mutex_lock")), // IDF: forever, pdTRUE/pdFALSE.
        ("_mutex_unlock", Gate("mutex_unlock")), // IDF: (handle), pdTRUE/pdFALSE.
        ("_queue_create", Gate("queue_create")), // IDF: (length, item bytes), handle or NULL.
        ("_queue_delete", Gate("queue_delete")), // IDF: (handle), void.
        ("_queue_send", Gate("osi_queue_send_to_back")), // IDF: send is send-to-back.
        ("_queue_send_from_isr", Gate("osi_queue_send_from_isr")), // IDF: word hptw; R4b/esp-radio use a byte.
        ("_queue_send_to_back", Gate("osi_queue_send_to_back")), // IDF: (queue, item, ticks), 1/0.
        ("_queue_send_to_front", Gate("osi_queue_send_to_front")), // IDF: (queue, item, ticks), 1/0.
        ("_queue_recv", Gate("osi_queue_recv")), // IDF: (queue, item out, ticks), 1/0.
        ("_queue_msg_waiting", Gate("queue_waiting")), // IDF: (handle), unsigned count.
        ("_event_group_create", Fault),
        ("_event_group_delete", Fault),
        ("_event_group_set_bits", Fault),
        ("_event_group_clear_bits", Fault),
        ("_event_group_wait_bits", Fault),
        ("_task_create_pinned_to_core", Gate("osi_task_create")), // IDF: stack bytes; ignore stacked core id.
        ("_task_create", Gate("osi_task_create")), // IDF: six register arguments, pdPASS on success.
        ("_task_delete", Gate("osi_task_delete")), // IDF: NULL means current; void.
        ("_task_delay", Gate("osi_task_delay")), // IDF: ticks; void.
        ("_task_ms_to_tick", Gate("osi_task_ms_to_tick")), // Brief requires ceil; IDF floors.
        ("_task_get_current_task", Gate("task_current")), // IDF: current handle.
        ("_task_get_max_priority", Const(BLOB_MAX_PRIORITY)), // IDF: count, not highest index.
        ("_malloc", Gate("malloc")), // IDF: (bytes), pointer or NULL.
        ("_free", Gate("free")), // IDF: (pointer), void; NULL is allowed.
        ("_event_post", Glue("event_post_glue")), // wifi_os_adapter.h:75; esp-radio os_adapter/mod.rs:657-679.
        ("_get_free_heap_size", Gate("heap_free_bytes")), // IDF: free internal heap bytes.
        ("_rand", Gate("osi_random")),
        ("_dport_access_stall_other_cpu_start_wrap", Fault),
        ("_dport_access_stall_other_cpu_end_wrap", Fault),
        ("_wifi_pm_sleep_lock_acquire", Const(0)), // Brief: no PM; IDF's wrapper is conditional.
        ("_wifi_pm_sleep_lock_release", Const(0)), // Brief: no PM; esp-radio can acquire/release a lock.
        // ESP-IDF v6.1.1 components/esp_wifi/esp32s3/esp_adapter.c:623-633,688-689.
        ("_phy_disable", Glue("phy_disable_glue")),
        ("_phy_enable", Glue("phy_enable_glue")),
        ("_phy_update_country_info", Const(0)), // Assigned ESP_OK, as esp-radio's no-op.
        ("_read_mac", Gate("osi_read_mac")),
        ("_timer_arm", Gate("timer_arm")), // IDF: (timer, milliseconds, repeat), void.
        ("_timer_disarm", Gate("timer_disarm")), // IDF: (timer), void; kernel status ignored.
        ("_timer_done", Gate("timer_done")), // IDF: (timer), void; kernel status ignored.
        ("_timer_setfn", Gate("timer_setfn")), // IDF: (timer, function, argument), void.
        ("_timer_arm_us", Gate("timer_arm_us")), // IDF: (timer, microseconds, repeat), void.
        ("_wifi_reset_mac", Gate("osi_wifi_reset_mac")),
        // IDF components/soc/esp32s3/register/soc/syscon_reg.h: SYSTEM_WIFI_CLK_WIFI_EN = 0.
        // esp-radio esp-radio/src/radio_clocks/clocks_ll/esp32s3.rs: enable_wifi is empty.
        ("_wifi_clock_enable", Const(0)),
        ("_wifi_clock_disable", Const(0)),
        ("_wifi_rtc_enable_iso", Const(0)), // IDF S3 wrapper: CONFIG_MAC_BB_PD off.
        ("_wifi_rtc_disable_iso", Const(0)), // IDF S3 wrapper: CONFIG_MAC_BB_PD off.
        ("_esp_timer_get_time", Gate64("osi_esp_timer_get_time")), // IDF: 64-bit microseconds.
        // NVS is never initialized and never gives out a handle.
        ("_nvs_set_i8", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_get_i8", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_set_u8", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_get_u8", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_set_u16", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_get_u16", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_open", Const(ESP_ERR_NVS_NOT_INITIALIZED)),
        ("_nvs_close", Const(0)),
        ("_nvs_commit", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_set_blob", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_get_blob", Const(ESP_ERR_NVS_INVALID_HANDLE)),
        ("_nvs_erase_key", Fault),
        // esp-radio esp-radio/src/wifi/os_adapter/mod.rs:1240-1246: any length, return 0.
        ("_get_random", Chunked("osi_random")),
        ("_get_time", Gate("osi_get_time")), // IDF: {sec, usec}, zero; esp-radio is TODO.
        ("_random", Gate("osi_random")),
        ("_slowclk_cal_get", Gate("osi_slowclk_cal_get")),
        ("_log_write", Const(0)), // The line is dropped until R6 routes it to Fab.
        ("_log_writev", Const(0)), // The line is dropped until R6 routes it to Fab.
        ("_log_timestamp", Gate("osi_log_timestamp")),
        ("_malloc_internal", Gate("malloc")), // IDF allocation signatures; this heap is internal.
        ("_realloc_internal", Realloc), // IDF: (pointer, bytes), pointer or NULL.
        ("_calloc_internal", Calloc), // IDF: (count, bytes), zeroed pointer or NULL.
        ("_zalloc_internal", Zalloc), // IDF: (bytes), zeroed pointer or NULL.
        ("_wifi_malloc", Gate("malloc")), // IDF: (bytes), pointer or NULL.
        ("_wifi_realloc", Realloc), // IDF: (pointer, bytes), pointer or NULL.
        ("_wifi_calloc", Calloc), // IDF: (count, bytes), zeroed pointer or NULL.
        ("_wifi_zalloc", Zalloc), // IDF: (bytes), zeroed pointer or NULL.
        ("_wifi_create_queue", Gate("osi_wifi_create_queue")), // IDF: {handle, storage}; esp-radio boxes handle.
        ("_wifi_delete_queue", Gate("osi_wifi_delete_queue")), // IDF: delete handle, then wrapper; void.
        ("_coex_init", Fault),
        ("_coex_deinit", Fault),
        // IDF components/esp_wifi/esp32s3/esp_adapter.c wrappers with
        // SW_COEXIST/EXTERNAL_COEX off: return 0/NULL, or do nothing for void.
        ("_coex_enable", Const(0)),
        ("_coex_disable", Const(0)),
        ("_coex_status_get", Const(0)),
        ("_coex_condition_set", Fault),
        ("_coex_wifi_request", Const(0)),
        ("_coex_wifi_release", Const(0)),
        ("_coex_wifi_channel_set", Const(0)),
        ("_coex_event_duration_get", Const(0)),
        ("_coex_pti_get", Const(0)),
        ("_coex_schm_status_bit_clear", Const(0)),
        ("_coex_schm_status_bit_set", Const(0)),
        ("_coex_schm_interval_set", Const(0)),
        ("_coex_schm_interval_get", Const(0)),
        ("_coex_schm_curr_period_get", Const(0)),
        ("_coex_schm_curr_phase_get", Const(0)), // NULL.
        ("_coex_schm_process_restart", Const(0)),
        ("_coex_schm_register_cb", Const(0)),
        ("_coex_register_start_cb", Const(0)),
        ("_coex_schm_flexible_period_set", Const(0)), // ESP_COEX_POWER_MANAGEMENT off.
        ("_coex_schm_flexible_period_get", Const(1)), // IDF #else with ESP_COEX_POWER_MANAGEMENT off.
        ("_coex_schm_get_phase_by_idx", Const(0)), // NULL.
        ("_magic", Data(OSI_MAGIC)), // IDF ESP_WIFI_OS_ADAPTER_MAGIC.
    ]
};

// ESP-IDF v6.1.1 esp_wifi_crypto_types.h:395-409. Keep the R3 row names and the
// debugger's static routine names together until R12 supplies the functions.
macro_rules! crypto_fields {
    ($($field:literal),+) => {
        pub const CRYPTO_FIELDS: [(&str, Entry); 11] = [$(($field, Entry::Fault)),+];
        pub const CRYPTO_STUB_NAMES: [&str; CRYPTO_FIELDS.len()] = [$(concat!("wifi_crypto_fault_", $field)),+];
    };
}
crypto_fields!("hmac_sha256_vector", "pbkdf2_sha1", "aes_128_encrypt", "aes_128_decrypt",
    "omac1_aes_128", "ccmp_decrypt", "ccmp_encrypt", "aes_gmac", "sha256_vector", "aes_wrap", "aes_unwrap");

pub struct Adapter {
    pub bytes: Vec<u8>,
    pub table: [u32; 120],
    pub crypto: [u32; CRYPTO_FIELDS.len()],
    /// (name, absolute address, byte size), excluding literals and padding.
    pub symbols: Vec<(String, u32, u32)>,
}

const LABELS_PER_ROW: usize = 6;

fn heap_row(a: &mut Asm, entry: Entry, first_label: usize, targets: [u32; 3]) {
    let [reserve, word, tail, byte, finish, done] = std::array::from_fn(|i| first_label + i);
    let call = |a: &mut Asm, target| { a.mov_reg_i32(A8, target); a.callx8(A8); };
    let copy = entry == Entry::Realloc;
    // CALL8 preserves a2..a7. Keep the old pointer in a2 until free, the byte
    // count in a4 and the result pointer in a5 across all thunk calls.
    if copy {
        let [request_bytes, realloc_reserve, _] = targets;
        a.bccz_reg_label(ccz::EQ, A2, reserve);
        a.mov_n(A10, A2);
        call(a, request_bytes);
        a.minu(A4, A3, A10);
        a.base.label_assign(reserve);
        a.mov_n(A10, A2); a.mov_n(A11, A3);
        call(a, realloc_reserve);
        a.mov_n(A5, A10);
        a.bccz_reg_label(ccz::EQ, A5, done);
        a.bcc_reg_reg_label(cc::EQ, A5, A2, done);
        a.bccz_reg_label(ccz::EQ, A2, done);
        a.mov_n(A7, A2);
    } else {
        a.mov_n(A10, A2);
        if entry == Entry::Calloc { a.mov_n(A11, A3); }
        call(a, targets[0]);
        a.mov_n(A5, A10);
        a.bccz_reg_label(ccz::EQ, A5, done);
        if entry == Entry::Calloc { a.mull(A4, A2, A3); } else { a.mov_n(A4, A2); }
        a.movi(A8, 0);
    }
    // Heap payloads are eight-aligned. Process full words and the final zero
    // to three bytes at the interrupt level restored by the reserve thunk.
    a.mov_n(A6, A5);
    a.bcci_reg_label(cc::LTU, A4, 4, tail);
    a.base.label_assign(word);
    if copy { a.l32i(A8, A7, 0); a.addi(A7, A7, 4); }
    a.s32i(A8, A6, 0); a.addi(A6, A6, 4); a.addi(A4, A4, -4);
    a.bcci_reg_label(cc::GEU, A4, 4, word);
    a.base.label_assign(tail);
    a.bccz_reg_label(ccz::EQ, A4, finish);
    a.base.label_assign(byte);
    if copy { a.l8ui(A8, A7, 0); a.addi(A7, A7, 1); }
    a.s8i(A8, A6, 0); a.addi(A6, A6, 1); a.addi(A4, A4, -1);
    a.bccz_reg_label(ccz::NE, A4, byte);
    a.base.label_assign(finish);
    if copy {
        let [_, _, free] = targets;
        a.mov_n(A10, A2);
        call(a, free);
    }
    a.base.label_assign(done);
    a.mov_n(A2, A5);
    a.retw_n();
}

fn stub(a: &mut Asm, entry: Entry, index: usize, targets: [u32; 3]) {
    a.entry(A1, 32);
    match entry {
        Entry::Const(value) => {
            if (-2048..=2047).contains(&value) { a.movi(A2, value); }
            else { a.mov_reg_i32(A2, value as u32); }
            a.retw_n();
        }
        Entry::Fault | Entry::Pending(_) => {
            a.movi(A2, index as i32);
            // IllegalInstructionCause=0 reaches user_vector's unhandled
            // branch, then R3 fault_entry; a2 survives its WB-relative dump.
            a.op24(0);
        }
        Entry::Chunked(_) => {
            let word = index * LABELS_PER_ROW;
            let done = word + 1;
            // CALL8 preserves a2..a7: keep buf/unsigned len in a2/a3. The
            // callee's a2 returns in a10; PS is restored between word calls.
            // esp-radio esp-radio/src/wifi/os_adapter/mod.rs:1240-1246,
            // MIT OR Apache-2.0: fill any length and return success.
            a.bccz_reg_label(ccz::EQ, A3, done);
            a.base.label_assign(word);
            a.mov_reg_i32(A8, targets[0]);
            a.callx8(A8);
            for byte in 0..4 {
                a.s8i(A10, A2, 0);
                a.addi(A2, A2, 1);
                a.addi(A3, A3, -1);
                if byte != 3 {
                    a.bccz_reg_label(ccz::EQ, A3, done);
                    a.op24(enc::srli(A10, A10, 8));
                }
            }
            a.bccz_reg_label(ccz::NE, A3, word);
            a.base.label_assign(done);
            a.movi(A2, 0);
            a.retw_n();
            // Own executed instructions, excluding callees: len=0 costs 5;
            // otherwise 4 fixed + 21 per full word + (5*r + 2) for a tail
            // of r=1..3 bytes. Forward bccz_reg_label emits BNEZ/J, while
            // the backward BNEZ is one instruction. Static body: 29 insns.
        }
        Entry::Calloc | Entry::Zalloc | Entry::Realloc => heap_row(a, entry, index * LABELS_PER_ROW, targets),
        _ => unreachable!("only stub entries have code"),
    }
}

/// `glue` resolves the absolute windowed entry, including the object's entry
/// offset. `thunk` resolves an ordinary R3 windowed thunk.
pub fn adapter(base: u32, thunk: impl Fn(&str) -> u32, glue: impl Fn(&str) -> u32) -> Adapter {
    assert_eq!(base & 3, 0, "adapter literal table requires word alignment");
    let mut out = Adapter { bytes: Vec::new(), table: [0; 120], crypto: [0; CRYPTO_FIELDS.len()], symbols: Vec::new() };
    let mut targets = [[0; 3]; OSI_FIELDS.len()];
    // Resolve each target once; assembly itself makes multiple passes.
    for (index, &(_, entry)) in OSI_FIELDS.iter().enumerate() {
        for (i, &name) in entry.kernel_exports().iter().enumerate() {
            targets[index][i] = thunk(name);
        }
        out.table[index] = match entry {
            Entry::Data(value) => value,
            Entry::Gate(_) | Entry::Gate64(_) => targets[index][0],
            Entry::Glue(target) => glue(target),
            _ => 0,
        };
    }
    out.bytes = assemble(&mut Asm::new(OSI_FIELDS.len() * LABELS_PER_ROW, A8), |a| {
        out.symbols.clear();
        a.literal_table();
        for (index, &(field, entry)) in OSI_FIELDS.iter().enumerate() {
            let kind = match entry {
                Entry::Const(_) => "const", Entry::Fault => "fault", Entry::Pending(_) => "pending",
                Entry::Chunked(_) => "chunked",
                Entry::Calloc => "calloc", Entry::Zalloc => "zalloc", Entry::Realloc => "realloc",
                _ => continue,
            };
            let start = a.base.offset();
            let address = base.checked_add(start).expect("adapter stub address overflow");
            out.table[index] = address;
            stub(a, entry, index, targets[index]);
            out.symbols.push((format!("osi_{kind}_{field}"), address, a.base.offset() - start));
        }
        for (i, &(_, entry)) in CRYPTO_FIELDS.iter().enumerate() {
            let start = a.base.offset();
            let address = base.checked_add(start).expect("crypto stub address overflow");
            out.crypto[i] = address;
            stub(a, entry, OSI_FIELDS.len() + i, [0; 3]);
            out.symbols.push((CRYPTO_STUB_NAMES[i].into(), address, a.base.offset() - start));
        }
    });
    base.checked_add(u32::try_from(out.bytes.len()).expect("adapter size overflow"))
        .expect("adapter address overflow");
    out
}

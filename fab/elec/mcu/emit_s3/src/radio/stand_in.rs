//! Fab's stand-in for the station pack exercises its documented interface in the simulator.
//! Fab chooses the call sequences independently of the pack's code and internal behaviour.
//! The Radio row describes this as “Simulated radio: Fab's stand-in. Espressif's code runs
//! only on the board.”
//!
//! The contracts are pinned to ESP-IDF v6.1.1 (Apache-2.0): esp_wifi.h:405-467 for the station
//! API, wifi_os_adapter.h:66-75 and esp32s3/esp_adapter.c:635-681 for OSI calls, and
//! phy_init.c:282-424 for PHY enable and disable.

use std::ops::Range;

use fab_elec_mcu_emu::bus::Width;
use fab_elec_mcu_island::IslandBinding;
use fab_elec_mcu_s3::{Ctx, ForeignBus, Next, VirtualFn, VirtualFns};

use super::{adapter::{OSI_FIELDS, OSI_MAGIC, OSI_VERSION}, kernel::PHY_RF_CAL_FULL, phy_glue::PHY_PACK_SYMBOLS,
    PHY_FUNS_OBJECT};
use crate::RadioSram;

pub const STATE_OBJECT: &str = "radio_stand_in_state";

// Word offsets in the restart-cleared BSS record.
pub const SIGNATURE: u32 = 0;
pub const MODE: u32 = 1;
pub const LOCK: u32 = 2;
pub const QUEUE: u32 = 3;
pub const QUEUE_HANDLE: u32 = 4;
pub const SEM: u32 = 5;
pub const TASK: u32 = 6;
pub const TIMER: u32 = 7;
pub const FAILED: u32 = 8;
pub const STARTED: u32 = 9;
pub const SEND: u32 = 10; // two words
pub const RECV: u32 = 12; // two words
pub const START_RESULT: u32 = 14;
pub const MAC: u32 = 15; // two words
pub const PHY_LAST: u32 = 17;
pub const PHY: u32 = 18;
pub const PHY_WORDS: u32 = 4; // count, then the last call's a10, a11 and a12
pub const STATE_BYTES: usize = (PHY as usize + PHY_WORDS as usize * PHY_PACK_SYMBOLS.len()) * 4;

pub const E_OSI: usize = 0;
pub const E_STATE: usize = 1;
pub const E_PP_TASK: usize = 2;
pub const E_MAC_ISR: usize = 3;
pub const E_TIMER_FN: usize = 4;
pub const E_EVENT_BASE: usize = 5;
pub const E_CAL_DATA: usize = 6;
pub const E_CONFIG: usize = 7;
pub const E_DIG_REGS: usize = 8;
pub const E_PHY_FUNS: usize = 9;
pub const ENV_WORDS: usize = E_PHY_FUNS + 1;

fn env<'a>(ctx: &Ctx<'a>) -> Option<&'a [u32; ENV_WORDS]> {
    ctx.env.try_into().ok()
}

const FRAME: u32 = 144; // the smallest frame VirtualFns::new takes; it refuses less
type Body = fn(&Ctx, &dyn ForeignBus) -> Next;

#[derive(Clone, Copy)]
enum Meaning { Entry(Body), Phy(usize), Callback(Body), NotYet }

const ROWS: &[(&str, Meaning)] = {
    use Meaning::{Callback, Entry, NotYet, Phy};
    &[
        ("esp_wifi_init_internal", Entry(init)),
        ("esp_wifi_set_mode", Entry(set_mode)),
        ("esp_wifi_get_mode", Entry(get_mode)),
        ("esp_wifi_start", Entry(start)),

        ("esp_wifi_deinit_internal", NotYet),
        ("esp_wifi_set_config", NotYet),
        ("esp_wifi_get_config", NotYet),
        ("esp_wifi_stop", NotYet),
        ("esp_wifi_connect_internal", NotYet),
        ("esp_wifi_disconnect_internal", NotYet),
        ("esp_wifi_scan_start", NotYet),
        ("esp_wifi_scan_stop", NotYet),
        ("esp_wifi_scan_get_ap_num", NotYet),
        ("esp_wifi_scan_get_ap_records", NotYet),
        ("esp_wifi_internal_tx", NotYet),
        ("esp_wifi_internal_reg_rxcb", NotYet),
        ("esp_wifi_internal_free_rx_buffer", NotYet),
        ("esp_wifi_get_mac", NotYet),
        ("esp_wifi_set_ps", NotYet),
        ("esp_wifi_sta_get_ap_info", NotYet),
        ("esp_wifi_set_country", NotYet),
        ("esp_wifi_set_storage", NotYet),
        ("esp_wifi_register_wpa_cb_internal", NotYet),
        ("esp_wifi_unregister_wpa_cb_internal", NotYet),
        ("esp_wifi_set_sta_key_internal", NotYet),
        ("esp_wifi_get_sta_key_internal", NotYet),
        ("esp_wifi_del_key_internal", NotYet),
        ("esp_wifi_set_appie_internal", NotYet),
        ("esp_wifi_unset_appie_internal", NotYet),
        ("esp_wifi_get_assoc_bssid_internal", NotYet),
        ("esp_wifi_get_macaddr_internal", NotYet),
        ("esp_wifi_sta_prof_is_wpa2_internal", NotYet),
        ("esp_wifi_sta_prof_is_rsn_internal", NotYet),
        ("esp_wifi_sta_get_prof_pmk_internal", NotYet),
        ("esp_wifi_sta_get_prof_ssid_internal", NotYet),
        ("esp_wifi_sta_get_prof_password_internal", NotYet),
        ("esp_wifi_sta_get_pairwise_cipher_internal", NotYet),
        ("esp_wifi_sta_get_group_cipher_internal", NotYet),
        ("esp_wifi_auth_done_internal", NotYet),
        ("esp_wifi_wpa_ptk_init_done_internal", NotYet),
        ("esp_wifi_register_eapol_txdonecb_internal", NotYet),
        ("esp_wifi_internal_osi_funcs_md5_check", NotYet),
        ("esp_wifi_internal_crypto_funcs_md5_check", NotYet),
        ("esp_wifi_internal_wifi_type_md5_check", NotYet),
        ("esp_wifi_internal_supplicant_header_md5_check", NotYet),
        ("phy_get_romfunc_addr", NotYet),
        ("phy_init_flag", NotYet),
        ("phy_get_rf_cal_version", NotYet),
        ("phy_set_wifi_mode_only", NotYet),
        ("phy_version_print", NotYet),

        // Each index selects both the record and the virtual function body.
        ("phy_init_param_set", Phy(PARAM)),
        ("phy_bbpll_en_usb", Phy(BBPLL)),
        ("register_chipv7_phy", Phy(REGISTER)),
        ("phy_wakeup_init", Phy(WAKE)),
        ("phy_dig_reg_backup", Phy(BACKUP)),
        ("phy_param_track_tot", Phy(TRACK)),
        ("phy_wifi_enable_set", Phy(WIFI)),
        ("phy_close_rf", Phy(CLOSE)),
        ("phy_xpd_tsens", Phy(TSENS)),
        ("phy_wait_freq_hw_hop_done", Phy(HOP_DONE)),

        ("ppTask", Callback(pp_task)),
        ("wDev_ProcessFiq", Callback(mac_isr)),
        ("pp_timer_process", Callback(timer_callback)),
    ]
};

/// All declared names, including rows a particular image cannot bind.
pub fn row_names() -> impl Iterator<Item = &'static str> {
    ROWS.iter().map(|&(name, _)| name)
}

fn not_yet(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
    if env(ctx).is_none() { return Next::Stop(NO_ENV); }
    Next::Stop("not simulated yet")
}

// Fab's choices for exercising the interface. These values do not describe the pack.
pub const SIM_SIGNATURE: u32 = u32::from_le_bytes(*b"FAB1");
const SIM_QUEUE_LEN: u32 = 2;
const SIM_MSG_BYTES: u32 = 8; // kind and the caller's semaphore
const SIM_MSG_START: u32 = 1;
pub const SIM_PP_STACK: u32 = 4096;
const SIM_PP_PRIORITY: u32 = 2; // OSI priority, before the kernel adds one
const SIM_CAL_CYCLES: u32 = 32;
pub const SIM_CAL_BYTE: u8 = 0xa5;
// Fab publishes g_phyFuns during register_chipv7_phy. The chip's writer is unknown.
// B-ROM0 checks the board's pointer. Ours is the start of the ROM-owned page,
// within runtime.rs transfer_phy_table's bounds for the 656-byte table. The stand-in never
// writes the ROM-owned page: the runtime snapshots and restores whatever the model holds there.
const SIM_PHY_FUNS: u32 = RadioSram::ROM_DATA_START;

// Unverified: esp_err.h is not in the pinned trees.
const ESP_ERR_WIFI_BASE: u32 = 0x3000;
const ESP_ERR_NO_MEM: u32 = 0x101;
const ESP_ERR_INVALID_ARG: u32 = 0x102;
const ESP_FAIL: u32 = u32::MAX;
const ESP_ERR_WIFI_NOT_INIT: u32 = ESP_ERR_WIFI_BASE + 1; // esp_wifi.h:63
const WIFI_INIT_CONFIG_MAGIC: u32 = 0x1f2f_3f4f; // esp_wifi.h:192
const WIFI_MODE_STA: u32 = 1;
const WIFI_MODE_MAX: u32 = 5; // esp_wifi_types_generic.h:27-32
const WIFI_EVENT_STA_START: u32 = 2; // esp_wifi_types_generic.h:1270-1273
const FOREVER: u32 = u32::MAX; // wifi_os_adapter.h:23
// esp_rom/esp32s3/include/esp32s3/rom/ets_sys.h:199-205: five 4-byte fields.
// The kernel's ETS_BYTES in objects.splash is also 20.
const ETS_TIMER_BYTES: u32 = 20;
const ETS_WIFI_MAC_INTR_SOURCE: u32 = 0; // ESP32-S3 soc/interrupts.h:18
const ETS_WMAC_INUM: u32 = 0; // ESP32-S3 soc/soc.h:212

const NO_ENV: &str = "the stand-in has no addresses for this image";
type Addresses = [u32; ENV_WORDS];
type Decision = Result<Next, &'static str>;

// Resolve field names at compile time, including the data words at either end of the table.
const fn osi_offset(name: &str) -> u32 {
    let wanted = name.as_bytes();
    let mut i = 0;
    while i < OSI_FIELDS.len() {
        let field = OSI_FIELDS[i].0.as_bytes();
        if field.len() == wanted.len() {
            let mut j = 0;
            while j < field.len() && field[j] == wanted[j] { j += 1; }
            if j == field.len() { return i as u32 * 4; }
        }
        i += 1;
    }
    panic!("unknown OSI field")
}

struct Record<'a> { image: &'a Addresses, bus: &'a dyn ForeignBus }

impl Record<'_> {
    fn at(&self, field: u32) -> u32 { self.image[E_STATE] + 4 * field }
    fn word(&self, field: u32) -> Result<u32, &'static str> {
        self.bus.sram_word(self.at(field)).ok_or("cannot read the stand-in record")
    }
    fn call<const N: usize>(&self, offset: u32, values: [u32; N]) -> Next {
        const { assert!(N <= 6) };
        let mut args = [0; 6];
        args[..N].copy_from_slice(&values);
        Next::Call { target_at: self.image[E_OSI] + offset, args }
    }
}

macro_rules! osi {
    ($r:expr, $name:literal $(, $arg:expr)*) => {
        $r.call(const { osi_offset($name) }, [$($arg),*])
    };
}

fn decide(ctx: &Ctx, bus: &dyn ForeignBus, step: impl FnOnce(&Record) -> Decision) -> Next {
    let Some(image) = env(ctx) else { return Next::Stop(NO_ENV); };
    if !bus.writable_frame(image[E_STATE], STATE_BYTES) {
        return Next::Stop("the stand-in record needs writable aligned SRAM");
    }
    if image[E_OSI].checked_add(4 * OSI_FIELDS.len() as u32).is_none() {
        return Next::Stop("the OSI table address wraps");
    }
    step(&Record { image, bus }).unwrap_or_else(Next::Stop)
}

// Every effect's destination is preflighted by the decision that commits it.
// No device event runs between the decision and its writes (foreign.rs).
fn put(ctx: &Ctx, bus: &mut dyn ForeignBus, field: u32, value: u32) {
    if let Some(image) = env(ctx) {
        let _ = bus.write(image[E_STATE] + field * 4, Width::W, value, 0);
    }
}

fn keep<const FIELD: u32>(ctx: &Ctx, bus: &mut dyn ForeignBus) {
    if let Some(value) = ctx.result {
        put(ctx, bus, FIELD, value);
        if value == 0 { put(ctx, bus, FAILED, ESP_ERR_NO_MEM); }
        if FIELD == QUEUE && value != 0 {
            if let Some(handle) = bus.sram_word(value) { put(ctx, bus, QUEUE_HANDLE, handle); }
        }
    }
}

fn init(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    decide(ctx, bus, |r| {
        let failed = r.word(FAILED)?;
        Ok(match ctx.step {
            0 => {
                if r.word(SIGNATURE)? != 0 { return Err("called again after init, which has no documented result"); }
                let config = ctx.args[0];
                let magic = config.checked_add(156).and_then(|p| bus.sram_word(p));
                if config & 3 != 0 || bus.sram_word(config).is_none() || magic.is_none() {
                    return Err("config needs 160 bytes of aligned SRAM");
                }
                if bus.sram_word(config) != Some(r.image[E_OSI]) { return Err("config has the wrong OSI table"); }
                if magic != Some(WIFI_INIT_CONFIG_MAGIC) { return Err("config magic is not WIFI_INIT_CONFIG_MAGIC"); }
                if bus.sram_word(r.image[E_OSI] + const { osi_offset("_version") }) != Some(OSI_VERSION) {
                    return Err("OSI table version mismatch");
                }
                if bus.sram_word(r.image[E_OSI] + const { osi_offset("_magic") }) != Some(OSI_MAGIC) {
                    return Err("OSI table magic mismatch");
                }
                Next::Effect(|c, b| put(c, b, FAILED, 0))
            }
            1..=20 if failed != 0 => Next::Burn(1),
            1 => osi!(r, "_recursive_mutex_create"),
            2 => Next::Effect(keep::<LOCK>),
            3 => osi!(r, "_wifi_create_queue", SIM_QUEUE_LEN, SIM_MSG_BYTES),
            4 => {
                if let Some(wrapper) = ctx.result.filter(|&p| p != 0) {
                    // kernel/adapter.splash:104-114: wrapper[0] is the queue handle.
                    if wrapper & 3 != 0 || bus.sram_word(wrapper).is_none() { return Err("unreadable Wi-Fi queue wrapper"); }
                }
                Next::Effect(keep::<QUEUE>)
            }
            5 => osi!(r, "_semphr_create", 1, 0),
            6 => Next::Effect(keep::<SEM>),
            7 => osi!(r, "_wifi_zalloc", ETS_TIMER_BYTES),
            8 => Next::Effect(keep::<TIMER>),
            9 => osi!(r, "_timer_setfn", r.word(TIMER)?, r.image[E_TIMER_FN], 0),
            10 => osi!(r, "_task_create_pinned_to_core", r.image[E_PP_TASK], 0, SIM_PP_STACK, 0,
                SIM_PP_PRIORITY, r.at(TASK)),
            11 if ctx.result != Some(1) => Next::Effect(|c, b| put(c, b, FAILED, ESP_ERR_NO_MEM)),
            11 => Next::Burn(1),
            12 => osi!(r, "_semphr_take", r.word(SEM)?, FOREVER),
            13 if ctx.result != Some(1) => return Err("the task readiness semaphore was not taken"),
            13 => osi!(r, "_semphr_delete", r.word(SEM)?),
            14 => Next::Effect(|c, b| put(c, b, SEM, 0)),
            // ets_sys.h:446-448: route the source, attach its handler, then enable the line.
            15 => osi!(r, "_set_intr", 0, ETS_WIFI_MAC_INTR_SOURCE, ETS_WMAC_INUM, 1),
            16 => osi!(r, "_set_isr", ETS_WMAC_INUM, r.image[E_MAC_ISR], 0),
            17 => osi!(r, "_ints_on", 1 << ETS_WMAC_INUM),
            18 => osi!(r, "_read_mac", r.at(MAC), 0),
            19 if ctx.result != Some(0) => return Err("reading the station MAC failed"),
            19 => Next::Effect(|c, b| {
                put(c, b, MODE, WIFI_MODE_STA); // esp_wifi.h:428: station is the default.
                put(c, b, SIGNATURE, SIM_SIGNATURE);
            }),
            20 => Next::Return(0),
            // The release tail undoes only successful allocations, in reverse order.
            21 if r.word(TIMER)? != 0 => osi!(r, "_timer_done", r.word(TIMER)?),
            22 if r.word(TIMER)? != 0 => osi!(r, "_free", r.word(TIMER)?),
            23 if r.word(SEM)? != 0 => osi!(r, "_semphr_delete", r.word(SEM)?),
            24 if r.word(QUEUE)? != 0 => osi!(r, "_wifi_delete_queue", r.word(QUEUE)?),
            25 if r.word(LOCK)? != 0 => osi!(r, "_mutex_delete", r.word(LOCK)?),
            21..=25 => Next::Burn(1),
            26 => Next::Effect(|c, b| {
                for field in [TIMER, SEM, QUEUE, QUEUE_HANDLE, LOCK, TASK] { put(c, b, field, 0); }
            }),
            27 => Next::Return(failed),
            _ => return Err("unexpected init step"),
        })
    })
}

fn set_mode(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    decide(ctx, bus, |r| Ok(match ctx.step {
        0 if r.word(SIGNATURE)? == 0 => Next::Return(ESP_ERR_WIFI_NOT_INIT),
        0 if ctx.args[0] >= WIFI_MODE_MAX => Next::Return(ESP_ERR_INVALID_ARG),
        0 if ctx.args[0] != WIFI_MODE_STA => return Err("a station-only pack's answer for another mode is not documented"),
        0 => osi!(r, "_mutex_lock", r.word(LOCK)?),
        1 if ctx.result != Some(1) => return Err("the mode mutex was not acquired"),
        1 => Next::Effect(|c, b| put(c, b, MODE, c.args[0])),
        2 => osi!(r, "_mutex_unlock", r.word(LOCK)?),
        3 => Next::Return(0),
        _ => return Err("unexpected set_mode step"),
    }))
}

fn get_mode(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    decide(ctx, bus, |r| Ok(match ctx.step {
        0 if r.word(SIGNATURE)? == 0 => Next::Return(ESP_ERR_WIFI_NOT_INIT),
        0 if ctx.args[0] == 0 => Next::Return(ESP_ERR_INVALID_ARG),
        0 | 1 if !bus.writable_frame(ctx.args[0], 4) => return Err("mode output needs a writable aligned SRAM word"),
        0 => osi!(r, "_mutex_lock", r.word(LOCK)?),
        1 if ctx.result != Some(1) => return Err("the mode mutex was not acquired"),
        1 => Next::Effect(|c, b| {
            if let Some(e) = env(c) {
                if let Some(mode) = b.sram_word(e[E_STATE] + MODE * 4) {
                    let _ = b.write(c.args[0], Width::W, mode, 0);
                }
            }
        }),
        2 => osi!(r, "_mutex_unlock", r.word(LOCK)?),
        3 => Next::Return(0),
        _ => return Err("unexpected get_mode step"),
    }))
}

fn start(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    decide(ctx, bus, |r| Ok(match ctx.step {
        0 if r.word(SIGNATURE)? == 0 => Next::Return(ESP_ERR_WIFI_NOT_INIT),
        0 | 1 if r.word(STARTED)? != 0 => return Err("called again after it started, which has no documented result"),
        0 => osi!(r, "_mutex_lock", r.word(LOCK)?),
        1 if ctx.result != Some(1) => return Err("the start mutex was not acquired"),
        1 => Next::Effect(|c, b| put(c, b, FAILED, 0)),
        2 => osi!(r, "_wifi_thread_semphr_get"),
        3 if ctx.result == Some(0) => Next::Effect(|c, b| put(c, b, FAILED, ESP_ERR_NO_MEM)),
        3 => Next::Effect(|c, b| {
            if let Some(sem) = c.result {
                put(c, b, SEND, SIM_MSG_START);
                put(c, b, SEND + 1, sem);
            }
        }),
        4..=7 if r.word(FAILED)? != 0 => Next::Burn(1),
        4 => osi!(r, "_queue_send", r.word(QUEUE_HANDLE)?, r.at(SEND), FOREVER),
        5 if ctx.result != Some(1) => Next::Effect(|c, b| put(c, b, FAILED, ESP_FAIL)),
        5 => Next::Burn(1),
        // The mutex protects SEND until this caller has taken its own message's semaphore.
        6 => osi!(r, "_semphr_take", r.word(SEND + 1)?, FOREVER),
        7 if ctx.result != Some(1) => return Err("the start completion semaphore was not taken"),
        7 if r.word(START_RESULT)? == 0 => Next::Effect(|c, b| put(c, b, STARTED, 1)),
        7 => Next::Burn(1),
        8 => osi!(r, "_mutex_unlock", r.word(LOCK)?),
        9 if r.word(FAILED)? != 0 => Next::Return(r.word(FAILED)?),
        9 => Next::Return(if r.word(START_RESULT)? == 0 { 0 } else { ESP_FAIL }),
        _ => return Err("unexpected start step"),
    }))
}

fn pp_task(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    decide(ctx, bus, |r| {
        if ctx.step == 0 { return Ok(osi!(r, "_semphr_give", r.word(SEM)?)); }
        // Posting, recording its result, and waking the sender are distinct decisions.
        Ok(match (ctx.step - 1) % 5 {
            0 => osi!(r, "_queue_recv", r.word(QUEUE_HANDLE)?, r.at(RECV), FOREVER),
            1 if ctx.result != Some(1) || r.word(RECV)? != SIM_MSG_START => return Err("an unknown message"),
            1 => osi!(r, "_phy_enable"),
            2 => osi!(r, "_event_post", r.image[E_EVENT_BASE], WIFI_EVENT_STA_START, 0, 0, FOREVER),
            3 => Next::Effect(|c, b| {
                if let Some(result) = c.result { put(c, b, START_RESULT, result); }
            }),
            _ => osi!(r, "_semphr_give", r.word(RECV + 1)?),
        })
    })
}

fn mac_isr(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
    if env(ctx).is_none() { return Next::Stop(NO_ENV); }
    Next::Stop("the Wi-Fi interrupt ran; the simulator does not raise it yet")
}

fn timer_callback(ctx: &Ctx, _: &dyn ForeignBus) -> Next {
    if env(ctx).is_none() { return Next::Stop(NO_ENV); }
    Next::Stop("a radio timer fired; the simulator arms none yet")
}

// Indices in PHY_PACK_SYMBOLS. Zero in PHY_LAST means no call since the BSS clear.
const PARAM: usize = 0;
const BBPLL: usize = 1;
const REGISTER: usize = 2;
const WAKE: usize = 3;
const BACKUP: usize = 4;
const TRACK: usize = 5;
const WIFI: usize = 6;
const CLOSE: usize = 7;
const TSENS: usize = 8;
const HOP_DONE: usize = 9;
const PHY_BODIES: [Body; PHY_PACK_SYMBOLS.len()] = [
    phy::<PARAM>, phy::<BBPLL>, phy::<REGISTER>, phy::<WAKE>, phy::<BACKUP>,
    phy::<TRACK>, phy::<WIFI>, phy::<CLOSE>, phy::<TSENS>, phy::<HOP_DONE>,
];

// Fab's record tags distinguish on/save and preserve an order failure until the
// next decision can stop. A rejected call still leaves its count and arguments.
const SIM_PHY_VARIANT: u32 = 1 << 8;
const SIM_PHY_BAD_ORDER: u32 = 1 << 31;
const fn phy_tag(index: usize, a10: u32) -> u32 {
    let variant = if (index == WIFI || index == BACKUP) && a10 == 1 { SIM_PHY_VARIANT } else { 0 };
    (index as u32 + 1) | variant
}

fn phy_order(index: usize, a10: u32, last: u32) -> bool {
    let after = |i| last == phy_tag(i, 0);
    let on = last == phy_tag(WIFI, 1);
    let off = after(WIFI);
    let enable = last == 0 || on || off || after(HOP_DONE);
    let calibrated = after(REGISTER) || after(WAKE) || after(BACKUP);
    match index {
        PARAM | WAKE => enable, // phy_init.c:285-301,336,908-910
        BBPLL => after(PARAM), // phy_init.c:908-910,944-946
        REGISTER => after(BBPLL), // phy_init.c:944-946,962
        BACKUP if a10 == 1 => off, // esp_adapter.c:631-632; phy_init.c:254-260,388-389
        BACKUP => after(WAKE), // phy_init.c:262-266,336-340
        TRACK => calibrated || enable, // phy_init.c:363-366; phy_common.c:83-122
        WIFI if a10 == 0 => on, // esp_adapter.c:626,631
        WIFI => calibrated || enable || after(TRACK), // esp_adapter.c:623-626
        CLOSE => last == phy_tag(BACKUP, 1) || off, // phy_init.c:254-260 (conditional save),388-389,410
        TSENS => after(CLOSE), // phy_init.c:410-414
        HOP_DONE => after(TSENS), // phy_init.c:413-420
        _ => false,
    }
}

const PHY_ORDER_RULES: [&str; PHY_PACK_SYMBOLS.len()] = [
    "parameter setup must begin a PHY enable",
    "USB BBPLL enable must follow parameter setup",
    "calibration must follow USB BBPLL enable",
    "wakeup must begin a PHY enable",
    "register save must follow Wi-Fi off and restore must follow wakeup",
    "PLL tracking is out of order for a PHY enable",
    "Wi-Fi on/off is out of order",
    "RF close must follow Wi-Fi off or a register save",
    "temperature sensor shutdown must follow RF close",
    "waiting for a frequency hop must follow temperature sensor shutdown",
];

fn record_phy<const I: usize, const VALID_ORDER: bool>(ctx: &Ctx, bus: &mut dyn ForeignBus) {
    let Some(e) = env(ctx) else { return; };
    let row = PHY + PHY_WORDS * I as u32;
    if let Some(count) = bus.sram_word(e[E_STATE] + row * 4) {
        put(ctx, bus, row, count.wrapping_add(1));
        for (n, arg) in ctx.args[..3].iter().enumerate() { put(ctx, bus, row + 1 + n as u32, *arg); }
        let tag = phy_tag(I, ctx.args[0]);
        put(ctx, bus, PHY_LAST, if VALID_ORDER { tag } else { tag | SIM_PHY_BAD_ORDER });
    }
}

// esp_phy_init.h:47-51: version[4], mac[6], opaque[1894].
const CAL_OPAQUE: u32 = 10;
const CAL_BYTES: usize = 1904;
fn finish_calibration(ctx: &Ctx, bus: &mut dyn ForeignBus) {
    if let Some(e) = env(ctx) {
        for offset in CAL_OPAQUE..CAL_BYTES as u32 {
            let _ = bus.write(e[E_CAL_DATA] + offset, Width::B, u32::from(SIM_CAL_BYTE), 0);
        }
        let _ = bus.write(e[E_PHY_FUNS], Width::W, SIM_PHY_FUNS, 0);
    }
}

fn phy<const I: usize>(ctx: &Ctx, bus: &dyn ForeignBus) -> Next {
    decide(ctx, bus, |r| {
        let [a10, a11, a12, ..] = ctx.args;
        if ctx.step == 0 {
            // Decide against the previous call before the one committing effect replaces it.
            return Ok(if phy_order(I, a10, r.word(PHY_LAST)?) {
                Next::Effect(record_phy::<I, true>)
            } else {
                Next::Effect(record_phy::<I, false>)
            });
        }
        if ctx.step == 1 {
            if r.word(PHY_LAST)? & SIM_PHY_BAD_ORDER != 0 { return Err(PHY_ORDER_RULES[I]); }
            match I {
                PARAM if a10 != 1 => return Err("combo module parameter must be 1"), // phy_init.c:908-910
                BBPLL if a10 != 1 => return Err("USB BBPLL enable must be true"), // phy_init.c:944-946
                REGISTER if a10 == 0 || a11 != r.image[E_CAL_DATA] || a12 > PHY_RF_CAL_FULL =>
                    return Err("invalid PHY init pointer, calibration buffer or calibration mode"), // esp_phy_init.h:58-60
                BACKUP if a10 > 1 || a11 != r.image[E_DIG_REGS] =>
                    return Err("invalid save/restore selector or digital register buffer"), // phy_init.c:254-266
                TRACK if (a10, a11) != (1, 0) => return Err("PLL tracking requires Wi-Fi only"), // phy_common.c:104-122
                WIFI if a10 > 1 => return Err("Wi-Fi enable must be zero or one"), // esp_adapter.c:623-633
                _ => {}
            }
            return Ok(if I == REGISTER { Next::Burn(SIM_CAL_CYCLES) } else { Next::Return(0) });
        }
        match (I, ctx.step) {
            (REGISTER, 2) => {
                if !bus.writable_frame(r.image[E_CAL_DATA], CAL_BYTES) {
                    return Err("the PHY calibration buffer needs writable SRAM");
                }
                if !bus.writable_frame(r.image[E_PHY_FUNS], 4) {
                    return Err("g_phyFuns needs a writable aligned SRAM word");
                }
                Ok(Next::Effect(finish_calibration))
            }
            (REGISTER, 3) => Ok(Next::Return(0)), // ESP_OK, phy_init.c:962-964
            _ => Err("unexpected PHY step"),
        }
    })
}

pub struct StandIn {
    pub ranges: Vec<Range<u32>>,
    pub functions: Vec<VirtualFn>,
    pub env: Vec<u32>,
}

impl StandIn {
    pub fn table(self) -> Result<VirtualFns, String> {
        if self.env.len() != ENV_WORDS {
            return Err(format!("the stand-in needs {ENV_WORDS} image addresses, got {}", self.env.len()));
        }
        Ok(VirtualFns::new(self.ranges, self.functions)?.with_env(self.env))
    }
}

pub fn stand_in(island: &IslandBinding, radio: &RadioSram) -> Result<StandIn, String> {
    let glue = |name: &str| island.glue.get(name).copied()
        .ok_or_else(|| format!("the stand-in needs glue {name}"));
    let callback = |name: &str| island.functions.get(name).map(|f| f.start)
        .ok_or_else(|| format!("the stand-in needs callback {name}"));
    let mut env = [0; ENV_WORDS];
    env[E_OSI] = radio.osi_table;
    env[E_STATE] = glue(STATE_OBJECT)?;
    env[E_PP_TASK] = callback("ppTask")?;
    env[E_MAC_ISR] = callback("wDev_ProcessFiq")?;
    env[E_TIMER_FN] = callback("pp_timer_process")?;
    env[E_EVENT_BASE] = glue("wifi_event_base_name")?;
    env[E_CAL_DATA] = radio.phy_cal_data;
    env[E_CONFIG] = radio.wifi_config.unwrap_or(0);
    env[E_DIG_REGS] = radio.phy_dig_regs;
    env[E_PHY_FUNS] = island.objects.get(PHY_FUNS_OBJECT)
        .ok_or_else(|| format!("the stand-in needs object {PHY_FUNS_OBJECT}"))?.start;

    let mut ranges = Vec::with_capacity(island.code.len());
    for section in &island.code {
        let end = section.start.checked_add(section.bytes)
            .ok_or_else(|| format!("pack code range at {:#010x} wraps", section.start))?;
        ranges.push(section.start..end);
    }
    let mut functions = Vec::new();
    for &(name, meaning) in ROWS {
        let symbol = match island.functions.get(name) {
            Some(f) if f.bytes >= 15 => f,
            _ if matches!(meaning, Meaning::NotYet) => continue,
            Some(f) => return Err(format!("{name} needs 15 bytes, has {}", f.bytes)),
            None => return Err(format!("the stand-in needs function {name}")),
        };
        let body: Body = match meaning {
            Meaning::Entry(body) | Meaning::Callback(body) => body,
            Meaning::Phy(index) => {
                if PHY_PACK_SYMBOLS.get(index) != Some(&name) {
                    return Err(format!("{name} is not PHY_PACK_SYMBOLS[{index}]"));
                }
                PHY_BODIES[index]
            }
            Meaning::NotYet => not_yet,
        };
        functions.push(VirtualFn { name: name.into(), address: symbol.start, bytes: symbol.bytes, frame: FRAME, body });
    }
    Ok(StandIn { ranges, functions, env: env.to_vec() })
}

//! The port table: the only way out of a firmware program. Numbered, typed
//! and versioned; the front end's table (`fab-elec-mcu`'s `PORTS`)
//! must equal it, and the simulator's HAL is its reference behaviour.
//!
//! Also the declaration forms a module's DECLS section uses.

/// A port parameter or result type: the module format's type codes.
pub const TY_F32: u8 = 0;
pub const TY_I32: u8 = 1;
pub const TY_BOOL: u8 = 2;

/// Bumped whenever a port changes or is added.
pub const PORT_TABLE_VERSION: u32 = 15;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PortDef {
    pub name: &'static str,
    pub params: &'static [u8],
    pub rets: &'static [u8],
}

/// `pin_write(pin, level)`: level 0 low, 1 high (any other value: high).
pub const PIN_WRITE: u16 = 0;
/// `pin_read(pin) -> level` (0 or 1).
pub const PIN_READ: u16 = 1;
/// `clock_us() -> i32`: the device clock in microseconds (wrapping).
pub const CLOCK_US: u16 = 2;
/// `clock_ms() -> i32`: the device clock in milliseconds (wrapping).
pub const CLOCK_MS: u16 = 3;
/// `timer_stop(timer)`.
pub const TIMER_STOP: u16 = 4;
/// `timer_start(timer)`: an `every` restarts its period from now, an `after`
/// fires once after its delay from now.
pub const TIMER_START: u16 = 5;
/// `timer_period(timer, us)`: a new period (delay), from its next firing on.
pub const TIMER_PERIOD: u16 = 6;
/// `loop_cap()`: the loop at the call's site reached its run-time cap and
/// stopped (the compiler emits it; a fault).
pub const LOOP_CAP: u16 = 7;
/// `pwm_duty(pin, level)`: level saturates to 0..=255; 0 low, 255 high.
pub const PWM_DUTY: u16 = 8;
/// `serial_byte(link) -> i32`: the byte (0..=255) of the receive event
/// being handled for `link`; -1 anywhere else.
pub const SERIAL_BYTE: u16 = 9;
/// `present(screen, src, sx, sy, w, h, x, y)`: draws a U16 buffer's pixels to the panel
/// in call order; the program cannot observe when. Refused windows are skipped.
pub const PRESENT: u16 = 10;
/// `fill_rect(display, x, y, w, h, color)`: ordered like `present`;
/// `color` is RGB565 in the low 16 bits.
pub const FILL_RECT: u16 = 11;
/// `copy(dst, doff, src, soff, n)`: overlap-safe copy of like-kind elements.
pub const COPY: u16 = 12;
/// `fill(dst, off, n, value)`: fill elements, truncating to the buffer kind.
pub const FILL: u16 = 13;
/// `adc_read(pin) -> i32`: the declared analog input's voltage, sampled during
/// the call against the chip's reference: `floor(v / vref * 2^bits)`, clamped
/// to `0..=max`.
pub const ADC_READ: u16 = 14;
/// `eth_send(buf, off, n) -> i32`: queue one Ethernet frame; 1 queued, 0 full.
/// Reads a U8 buffer. Invalid lengths or ranges fault and return 0 without sending.
pub const ETH_SEND: u16 = 15;
/// `eth_recv(buf, off) -> i32`: copy the oldest frame; its length, or 0 empty.
/// The writable U8 buffer must have room for `watch::ETH_MAX` bytes at off.
/// An invalid range faults and returns 0, leaving the oldest frame queued.
pub const ETH_RECV: u16 = 16;
/// `eth_mac(buf, off)`: write the board's six-byte MAC to a writable U8 buffer.
/// An invalid range faults without writing. All eth ports require an eth declaration.
pub const ETH_MAC: u16 = 17;
/// `shift_out(data, clock, value, bits)`: both pins must be declared outputs.
/// `bits` must be 1..=32; otherwise `watch::FAULT_SHIFT_BITS` reports the count
/// and nothing is driven. Before the first bit, clock goes low. Then, most
/// significant first (bit `bits - 1` down to bit 0 of `value`), data takes the
/// bit, clock goes high, then clock goes low. Data is stable at every rising
/// clock edge. At the end clock is low and data holds bit 0.
/// The order and final levels are fixed; edge timing is the backend's choice,
/// and a backend may use a hardware shifter.
pub const SHIFT_OUT: u16 = 18;
/// `accel(axis) -> i32`: axis 0..2 in sensor axes, raw signed 16-bit counts
/// from the latest motion sample, zero before the first. An invalid axis
/// reports FAULT_RANGE and returns zero. Reads outside the handler use the
/// same latched sample. ACCEL_LSB_PER_G is generated from the board profile.
pub const ACCEL: u16 = 19;
/// `gyro(axis) -> i32`: as accel, with GYRO_LSB_PER_DPS as the scale.
/// Both ports require a motion declaration and never initiate bus traffic.
pub const GYRO: u16 = 20;
/// `touch_x() -> i32`: last delivered X in visible-glass pixels, origin top-left,
/// axes aligned with the display. Zero before the first report; release keeps
/// the last coordinates. All touch ports require a touch declaration, read
/// the same latch outside the handler and never initiate bus traffic.
pub const TOUCH_X: u16 = 21;
/// `touch_y() -> i32`: the Y coordinate of the same latched report.
pub const TOUCH_Y: u16 = 22;
/// `touch_down() -> i32`: one while a finger is reported, otherwise zero.
pub const TOUCH_DOWN: u16 = 23;

/// `eeprom_read(addr) -> i32`: `addr` is in `0..Profile.eeprom` (bytes).
/// Returns 0..=255: the last value written there; a new chip reads 0xFF.
/// For both EEPROM ports, a computed out-of-range address raises
/// `watch::FAULT_EEPROM_ADDR` at the call's site with arg = addr. Nothing is
/// written, and a read returns 0. A constant out-of-range address is refused
/// at load, as are both ports on a profile with no EEPROM (`eeprom == 0`).
pub const EEPROM_READ: u16 = 24;
/// `eeprom_write(addr, value)`: stores the low 8 bits at `addr`, within
/// `0..Profile.eeprom`. Every later read sees it; invalid addresses follow
/// `EEPROM_READ`'s fault/refusal contract.
/// A write can take milliseconds on a chip with real EEPROM (3.4 ms on the
/// ATmega328P). A backend may return once the write has started; the next
/// EEPROM access then waits for it. In a module that writes, every EEPROM
/// access counts this wait in the handler's cost.
pub const EEPROM_WRITE: u16 = 25;
/// `probe(id, value)`: sends the low 16 bits of `value` to the host as probe `id` (its low 8 bits),
/// one watch `Probe` frame per call, so a board can say what it computed: the same frames come from
/// the chip and from its emulator. It changes nothing on the device. The AVR has no `probe` yet.
pub const PROBE: u16 = 26;
/// The networking ports (`crate::net` has their vocabulary). `c` is a connection, a `net`
/// declaration's handle; any other value faults with `watch::FAULT_NET_HANDLE` (arg = c) and the
/// port returns 0. All of them require a `net` declaration.
/// `net_link() -> i32`: the link, `net::LINK_NONE`, `LINK_JOINING` or `LINK_UP`.
pub const NET_LINK: u16 = 27;
/// `net_link_error() -> i32`: the `net::code` of the last link loss or failed join.
pub const NET_LINK_ERROR: u16 = 28;
/// `net_link_detail() -> i32`: its raw source number (a Wi-Fi reason), for display.
pub const NET_LINK_DETAIL: u16 = 29;
/// `net_open(c) -> i32`: 1 the connection is now wanted (resolving the name and joining the
/// network are stages of the open), 0 it already was. The ports layer reopens a wanted connection
/// after each failure or loss, 1 s doubling to 10 s, reset by ready.
pub const NET_OPEN: u16 = 30;
/// `net_close(c) -> i32`: 1 the connection is no longer wanted (it is offline now, its queues
/// cleared, code 0), 0 it already was not.
pub const NET_CLOSE: u16 = 31;
/// `net_state(c) -> i32`: `net::OFFLINE`, `CONNECTING` or `READY`.
pub const NET_STATE: u16 = 32;
/// `net_epoch(c) -> i32`: how many ready periods of `c` have ended (by loss or by `net_close`).
/// An end clears both of `c`'s queues; nothing is delivered twice.
pub const NET_EPOCH: u16 = 33;
/// `net_error(c) -> i32`: the `net::code` of `c`'s last failure or end (0 after `net_close`).
pub const NET_ERROR: u16 = 34;
/// `net_detail(c) -> i32`: its raw number (DNS rcode, HTTP status, close code), for display.
pub const NET_DETAIL: u16 = 35;
/// `net_send(c, buf, off, n, flags) -> i32`: copies up to `n` bytes of a U8 buffer into `c`'s
/// declared send queue and returns how many it took (0: full or not ready; try later). `flags`:
/// `net::SEND_BINARY` (else text) set by a message's first send, `net::SEND_MORE` (the message
/// goes on in the next send). A message ends with the send that is taken whole without MORE; a
/// send with `n` 0 and no MORE ends an open message; empty messages are not sent. A negative `n`
/// or a range outside the buffer faults with `watch::FAULT_NET_RANGE` (arg = off), taking nothing.
pub const NET_SEND: u16 = 36;
/// `net_recv(c, buf, off, cap) -> i32`: copies up to `cap` next bytes of the oldest received
/// message into a writable U8 buffer, never past its end, freeing receive slots as it goes;
/// returns the count (0: nothing to read now). Bad ranges fault as for `net_send`, reading nothing.
pub const NET_RECV: u16 = 37;
/// `net_left(c) -> i32`: the oldest message's bytes still to read in parts already received, plus
/// 1 while its last part has not arrived; 0 with no message.
pub const NET_LEFT: u16 = 38;
/// `net_kind(c) -> i32`: the oldest message's kind, 1 binary, 0 text (0 with no message).
pub const NET_KIND: u16 = 39;
/// `play(buf, off, n) -> taken`: queue as many of the n mono I16 samples as fit, without waiting.
/// For play and record, a range outside the buffer moves nothing and returns zero. A computed
/// bad range faults at the call site. A statically impossible range is refused at load.
pub const PLAY: u16 = 40;
/// `speaker_free() -> n`: room in the playback queue, in mono samples.
pub const SPEAKER_FREE: u16 = 41;
/// `speaker_queued() -> n`: queued samples that have yet to clock out.
pub const SPEAKER_QUEUED: u16 = 42;
/// `speaker_on(level)`: 1 unmutes before enabling the amps, 0 disables them before muting.
/// The clocks keep running. These four ports require a speaker declaration.
pub const SPEAKER_ON: u16 = 43;
/// `record(buf, off, n) -> got`: copy up to n oldest unread samples to a writable I16 buffer
/// and advance the read cursor by the count copied. All buffer offsets and counts are in samples.
pub const RECORD: u16 = 44;
/// `mic_ready() -> n`: unread samples from the read cursor to the newest.
pub const MIC_READY: u16 = 45;
/// `mic_rewind(n) -> moved`: move back by up to n samples still held in history.
/// These three ports require a mic declaration.
pub const MIC_REWIND: u16 = 46;

pub const PORTS: &[PortDef] = &[
    PortDef { name: "pin_write", params: &[TY_I32, TY_I32], rets: &[] },
    PortDef { name: "pin_read", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "clock_us", params: &[], rets: &[TY_I32] },
    PortDef { name: "clock_ms", params: &[], rets: &[TY_I32] },
    PortDef { name: "timer_stop", params: &[TY_I32], rets: &[] },
    PortDef { name: "timer_start", params: &[TY_I32], rets: &[] },
    PortDef { name: "timer_period", params: &[TY_I32, TY_I32], rets: &[] },
    PortDef { name: "loop_cap", params: &[], rets: &[] },
    PortDef { name: "pwm_duty", params: &[TY_I32, TY_I32], rets: &[] },
    PortDef { name: "serial_byte", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "present", params: &[TY_I32, TY_I32, TY_I32, TY_I32, TY_I32, TY_I32, TY_I32, TY_I32], rets: &[] },
    PortDef { name: "fill_rect", params: &[TY_I32, TY_I32, TY_I32, TY_I32, TY_I32, TY_I32], rets: &[] },
    PortDef { name: "copy", params: &[TY_I32; 5], rets: &[] },
    PortDef { name: "fill", params: &[TY_I32; 4], rets: &[] },
    PortDef { name: "adc_read", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "eth_send", params: &[TY_I32; 3], rets: &[TY_I32] },
    PortDef { name: "eth_recv", params: &[TY_I32; 2], rets: &[TY_I32] },
    PortDef { name: "eth_mac", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "shift_out", params: &[TY_I32; 4], rets: &[] },
    PortDef { name: "accel", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "gyro", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "touch_x", params: &[], rets: &[TY_I32] },
    PortDef { name: "touch_y", params: &[], rets: &[TY_I32] },
    PortDef { name: "touch_down", params: &[], rets: &[TY_I32] },
    PortDef { name: "eeprom_read", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "eeprom_write", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "probe", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "net_link", params: &[], rets: &[TY_I32] },
    PortDef { name: "net_link_error", params: &[], rets: &[TY_I32] },
    PortDef { name: "net_link_detail", params: &[], rets: &[TY_I32] },
    PortDef { name: "net_open", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "net_close", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "net_state", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "net_epoch", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "net_error", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "net_detail", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "net_send", params: &[TY_I32; 5], rets: &[TY_I32] },
    PortDef { name: "net_recv", params: &[TY_I32; 4], rets: &[TY_I32] },
    PortDef { name: "net_left", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "net_kind", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "play", params: &[TY_I32; 3], rets: &[TY_I32] },
    PortDef { name: "speaker_free", params: &[], rets: &[TY_I32] },
    PortDef { name: "speaker_queued", params: &[], rets: &[TY_I32] },
    PortDef { name: "speaker_on", params: &[TY_I32], rets: &[] },
    PortDef { name: "record", params: &[TY_I32; 3], rets: &[TY_I32] },
    PortDef { name: "mic_ready", params: &[], rets: &[TY_I32] },
    PortDef { name: "mic_rewind", params: &[TY_I32], rets: &[TY_I32] },
];

/// Declaration forms (a DECLS entry's `form`), with their argument shapes.
/// `output(pin)`: handle = pin.
pub const FORM_OUTPUT: u32 = 0;
/// `input(pin, pull)`: pull 0 none, 1 up, 2 down; handle = pin.
pub const FORM_INPUT: u32 = 1;
/// `every(period_us, fn)`: period > 0; handle = timer number.
pub const FORM_EVERY: u32 = 2;
/// `after(delay_us, fn)`: delay >= 0, fires once at start; handle = timer number.
pub const FORM_AFTER: u32 = 3;
/// `pwm(pin, mhz)`: mhz > 0 (millihertz); handle = pin.
pub const FORM_PWM: u32 = 4;
/// `serial(baud, fn)`: baud > 0; handle = ordinal among serial declarations.
pub const FORM_SERIAL: u32 = 5;
/// `display()`: handle = ordinal; at most one display in v1.
pub const FORM_DISPLAY: u32 = 6;
/// `image(n, w, h)`: read-only U16 surface; handle = buffer ordinal.
/// Index n and dimensions name its pixels in IMAGES.
pub const FORM_IMAGE: u32 = 7;
/// `bytes(n)`: n writable U8 elements; handle = buffer ordinal.
pub const FORM_BYTES: u32 = 8;
/// `samples(n)`: n writable I16 elements; handle = buffer ordinal.
pub const FORM_SAMPLES: u32 = 9;
/// `words(n)`: n writable word elements; handle = buffer ordinal.
pub const FORM_WORDS: u32 = 10;
/// `surface(w, h)`: w * h writable U16 pixels; handle = buffer ordinal.
pub const FORM_SURFACE: u32 = 11;
/// `analog(pin)`: handle = pin.
pub const FORM_ANALOG: u32 = 12;
/// `eth(count, fn)`: count >= 1 receive buffers; handle = ordinal (only zero).
/// Each runtime-owned buffer holds up to `watch::ETH_MAX` bytes.
/// On the S3 watch link, frames in flight = slots: size for
/// `slots * 1514 >= down_bytes_per_second * credit_round_trip_seconds`.
/// Credits retain wire order, so a full TX ring adds up to
/// `ring_bytes / up_bytes_per_second` to the round trip. The adverse model
/// services one 64-byte packet each way per ms: an 8192-byte ring at the
/// 32000 B/s mic rate adds 256 ms, a maximum encoded frame takes 24 ms,
/// and the credit needs one more service. At 38000 B/s down this gives
/// `ceil(38000 * 0.281 / 1514) = 8` slots (12112 payload bytes, plus metadata).
/// This conservative budget uses the mic rate as a drain-rate floor; replace
/// the service assumption with the first board measurement and size again.
pub const FORM_ETH: u32 = 13;
/// `motion(period_us, fn)`: period > 0, at most one; handle = 0.
/// Overdue events coalesce and report FAULT_OVERRUN, as ordinary timers do.
pub const FORM_MOTION: u32 = 14;
/// `touch(period_us, fn)`: period 1..=300_000, at most one; handle = 0.
/// Appended after ordinary timers and motion; overdue events coalesce and
/// report FAULT_OVERRUN. Only a changed report invokes the handler.
pub const FORM_TOUCH: u32 = 15;
/// `net(kind, slots, send_bytes, fn)`: connection `c` = its ordinal among `net` declarations.
/// `kind` 0 WebSocket, 1 TCP stream; `slots` >= 2 receive slots of `net::NET_PART` bytes, this
/// connection's alone; `send_bytes` >= 1 its send queue. `fn` runs on any change of the link, the
/// state, data or credit (coalesced: state is read, not queued). The front end and the loader
/// both refuse other arguments.
pub const FORM_NET: u32 = 16;
/// `speaker(rate_hz, queue_ms, fn)`: at most one, handle 0. Start muted with amps off.
/// queue_ms is 20..=200 in steps of 5 ms. Refill at half full or less.
/// Overdue refill events coalesce like timers.
pub const FORM_SPEAKER: u32 = 17;
/// `mic(rate_hz, history_ms, fn)`: at most one, handle 0. Deliver a block event every
/// rate_hz / 50 samples. history_ms is 20..=640 in steps of 20 ms.
/// Both declarations use the same board-supported rate.
pub const FORM_MIC: u32 = 18;
pub const FORMS: u32 = 19;

// At 48 kHz, 200 ms of stereo I16 TX needs 38,400 bytes plus 40 twelve-byte descriptors.
// 640 ms of mono I16 history adds 61,440 bytes. Together these use less than 100 KiB of
// the S3's 384 KiB pool, leaving room for RX DMA, program buffers, state and stack.
pub const SPEAKER_MAX_MS: i32 = 200;
pub const MIC_MAX_MS: i32 = 640;

pub const PULL_NONE: i32 = 0;
pub const PULL_UP: i32 = 1;
pub const PULL_DOWN: i32 = 2;

/// Kernel numbers are separate from the travelling app ABI. Kernel modules
/// are compiled from in-repo sources at image build time and need no table
/// version of their own. Keep PORTS and PORT_TABLE_VERSION unchanged.
pub const KERNEL_BASE: u16 = 64;
pub const LOAD8U: u16 = 64;
pub const LOAD16U: u16 = 65;
pub const LOAD32: u16 = 66;
pub const STORE8: u16 = 67;
pub const STORE16: u16 = 68;
pub const STORE32: u16 = 69;
pub const COPY_RAW: u16 = 70;
pub const FILL_RAW: u16 = 71;
pub const CLZ: u16 = 72;
pub const IRQ_ENABLE: u16 = 73;
pub const IRQ_DISABLE: u16 = 74;
pub const IRQ_RAISE: u16 = 75;
pub const NOW64: u16 = 76;
pub const KERNEL_LOOP_CAP: u16 = 77;
/// Non-fatal kernel report: `report(code, arg)` emits `Event::Fault { code,
/// site, arg }` on the watch stream at the faults level and the kernel carries
/// on. `code` must be a compile-time constant in the kernel report block.
pub const KERNEL_REPORT: u16 = 78;
pub const PACK_LOAD32: u16 = 79;
pub const PACK_STORE32: u16 = 80;
pub const PACK_STORE8: u16 = 81;
pub const PACK_COPY: u16 = 82;
pub const PACK_FILL: u16 = 83;
pub const DEV_LOAD32: u16 = 84;
pub const DEV_STORE32: u16 = 85;
/// `radio_stop(cause)` stops the radio from an island export and suspends it.
/// The cause must be a compile-time constant in the kernel report block
/// (96..=127), which keeps 0 out: the stop word's zero means no stop is pending.
/// A cause shares the report block's watch names, on purpose.
pub const RADIO_STOP: u16 = 86;
pub const FORM_EXPORT: u32 = 64;
// The domains are split by number: the app tables may grow, but never into
// the kernel's numbers, which `load` refuses and `load_kernel` requires.
const _: () = assert!(PORTS.len() <= KERNEL_BASE as usize && FORMS <= FORM_EXPORT);

/// Indexed by port number minus KERNEL_BASE, never appended to PORTS.
/// PORT_TABLE_VERSION does not change for them: kernel modules ship with their runtime and the app loader refuses kernel ports.
/// The unprefixed memory ports (K) reach Kernel regions. The `pack_*` ports (P)
/// reach Kernel and Pack regions, and the `dev_*` ports (D) reach Device regions.
pub const KERNEL_PORTS: &[PortDef] = &[
    PortDef { name: "load8u", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "load16u", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "load32", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "store8", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "store16", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "store32", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "copy_raw", params: &[TY_I32; 4], rets: &[] },
    PortDef { name: "fill_raw", params: &[TY_I32; 4], rets: &[] },
    PortDef { name: "clz", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "irq_enable", params: &[TY_I32], rets: &[] },
    PortDef { name: "irq_disable", params: &[TY_I32], rets: &[] },
    PortDef { name: "irq_raise", params: &[TY_I32], rets: &[] },
    PortDef { name: "now64", params: &[TY_I32], rets: &[] },
    PortDef { name: "loop_cap", params: &[], rets: &[] },
    PortDef { name: "report", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "pack_load32", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "pack_store32", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "pack_store8", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "pack_copy", params: &[TY_I32; 4], rets: &[] },
    PortDef { name: "pack_fill", params: &[TY_I32; 4], rets: &[] },
    PortDef { name: "dev_load32", params: &[TY_I32], rets: &[TY_I32] },
    PortDef { name: "dev_store32", params: &[TY_I32; 2], rets: &[] },
    PortDef { name: "radio_stop", params: &[TY_I32], rets: &[] },
];
const _: () = assert!(KERNEL_PORTS.len() == (RADIO_STOP - KERNEL_BASE + 1) as usize);

/// Inclusive signed bounds of a device port result. Unknown ports/results stay
/// full. `analog_max` is the device profile's `AnalogTable::max()`.
pub fn ret_interval(port: u16, ret: usize, analog_max: i32) -> (i32, i32) {
    match (port, ret) {
        (PIN_READ, 0) => (0, 1),
        (ADC_READ, 0) => (0, analog_max),
        (EEPROM_READ, 0) => (0, 255),
        _ => (i32::MIN, i32::MAX),
    }
}

/// A pin's role as the source declares it (`output`, `input`, `pwm`, `analog`): how a host knows,
/// before a run, which pins the firmware drives, reads and pulls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinRole {
    Output,
    /// `pull`: 0 none, 1 up, 2 down (`FORM_INPUT`).
    Input { pull: i32 },
    Pwm,
    Analog,
}

/// The pin a declaration names and the role it gives it; None for a form that names no pin.
pub fn pin_role(form: u32, args: &[crate::module::DeclArg]) -> Option<(i32, PinRole)> {
    use crate::module::DeclArg::Int;
    match (form, args) {
        (FORM_OUTPUT, [Int(pin), ..]) => Some((*pin, PinRole::Output)),
        (FORM_INPUT, [Int(pin), Int(pull), ..]) => Some((*pin, PinRole::Input { pull: *pull })),
        (FORM_INPUT, [Int(pin)]) => Some((*pin, PinRole::Input { pull: 0 })),
        (FORM_PWM, [Int(pin), ..]) => Some((*pin, PinRole::Pwm)),
        (FORM_ANALOG, [Int(pin), ..]) => Some((*pin, PinRole::Analog)),
        _ => None,
    }
}

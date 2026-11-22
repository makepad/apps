//! Watch output. [`watch`] is read-only: no bootloader commands or serial writes. [`tether`] also
//! writes what a host [`Peer`] queues on the device's frame channel (FC1 ETH frames).

use makepad_serial::line::{Line, Step};
use fab_elec_mcu_netgw::Link;
use fab_elec_mcu_rt::watch::{
    cobs_decode, decode, Event, Frame, Unwrap, MAX_BODY, MAX_WIRE, FAULT_LOOP_CAP, FAULT_OVERRUN, FAULT_PIN, FAULT_TIMER,
    FAULT_SERIAL, FAULT_SERIAL_LOST, FAULT_SERIAL_LOST_AT_LEAST, FAULT_WATCHDOG, FAULT_DRAW, FAULT_PANEL, FAULT_RANGE, FAULT_SHIFT_BITS,
    FAULT_I2C_NACK, FAULT_I2C_TIMEOUT, FAULT_EEPROM_ADDR, FAULT_KERNEL_BLOCK_IN_CRITICAL,
    FAULT_KERNEL_TIMER_REFUSED, FAULT_KERNEL_CRIT_MISMATCH, FAULT_KERNEL_CRIT_SPAN, FAULT_KERNEL_ISR_DEPTH,
    FAULT_RADIO_PHY_INIT_FAILED, FAULT_RADIO_PHY_CAL_STORE_FAILED, FAULT_KERNEL_IRQ_LINE_REFUSED,
    FAULT_KERNEL_IRQ_SOURCE_REFUSED, FAULT_KERNEL_ISR_TABLE_FULL, FAULT_KERNEL_ISR_ENTRY_REFUSED,
    FAULT_RADIO_ISLAND_MISMATCH, FAULT_KERNEL_TIMER_LOST, FAULT_KERNEL_TIMER_UNSET, FAULT_KERNEL_HEAP_EXHAUSTED,
    FAULT_KERNEL_TIMER_HANDLE_INVALID, FAULT_RADIO_STARTED, FAULT_RADIO_PHY_TABLE_REFUSED,
    FAULT_RADIO_EVENT, FAULT_RADIO_START_FAILED, NET_CLOSE, NET_DATA, NET_OPEN,
};
use std::{io::{self, Write}, time::{Duration, Instant}};

/// A ROM text line longer than this prints in pieces.
const ROM_LINE: usize = 256;

#[derive(Clone, Copy, Debug)]
pub enum Reset {
    /// A 50 ms DTR pulse (a USB-serial bridge's auto-reset), then the input already there is dropped.
    Dtr,
    /// A flash-boot reset through an ESP32-S3's USB Serial/JTAG with the port kept open, every byte
    /// after it kept (the ROM's text prints as `rom` lines). The controller acts on (RTS, DTR) states:
    /// (0,0) clears the download flag, (0,1) sets it, (1,0) resets the chip, (1,1) does nothing (TRM
    /// §33.3.2 Table 33.3-2), and a DTR change takes effect with the next RTS write (the sequences'
    /// repeated RTS writes propagate DTR). So flash boot is the table's own order, clear DTR, clear RTS,
    /// set RTS, clear RTS (§33.4.2 Table 33.4-4): the first RTS write clears the flag whatever the port
    /// left. Clearing RTS first sets it from an open port's (1,1) and the reset goes to download mode
    /// (the board showed boot:0x23, 11-18). The 100 ms between setting and clearing RTS is a lead choice.
    Usj,
    None,
}

/// Read for `limit` after reset/drain, or until disconnection when it is None.
/// `reset_ms` measures the reset's first control change to the Hello packet's first received byte;
/// with Reset::None it measures from entry instead. Serial reads and timestamps
/// share Line's clock, allowing the entire session to run under virtual time.
/// The summary counts all valid/error packets, even those preceding Hello;
/// Lost totals count only the stream accepted after Hello. A saturated Lost
/// count makes that total a lower bound, reported as `lost_saturated=1`.
pub fn watch(
    line: &mut impl Line,
    reset: Reset,
    limit: Option<Duration>,
    output: &mut dyn Write,
) -> io::Result<()> {
    watch_until(line, reset, limit, output, || false)
}

/// Like [`watch`], but also finishes with a summary when `stopped` is true.
/// The callback is checked between reads, at most one second apart on an idle
/// port. This lets a command handle interruption without signal handling in
/// the library or serial transport.
pub fn watch_until(
    line: &mut impl Line,
    reset: Reset,
    limit: Option<Duration>,
    output: &mut dyn Write,
    stopped: impl FnMut() -> bool,
) -> io::Result<()> {
    run(line, reset, limit, output, stopped, None)
}

/// A host program on the device's frame channel. [`tether`] hands it each frame printed after the
/// first Hello and lets it queue Ethernet frames at least every [`TURN`]; the link sends them as
/// the device's credits allow. A Hello starts a new link lifetime and drops what was queued.
pub trait Peer {
    /// `now` runs from the reset; `frame` is the frame just printed, if any.
    fn turn(&mut self, link: &mut Link, now: Duration, frame: Option<&Frame>);
    /// The peer's line for the summary.
    fn summary(&self, output: &mut dyn Write) -> io::Result<()>;
}

/// The longest a peer waits for a turn on an idle port.
pub const TURN: Duration = Duration::from_millis(20);

/// Like [`watch`], and `peer` talks to the firmware: what it queues is written to the port.
pub fn tether(
    line: &mut impl Line,
    reset: Reset,
    limit: Option<Duration>,
    output: &mut dyn Write,
    peer: &mut dyn Peer,
) -> io::Result<()> {
    run(line, reset, limit, output, || false, Some(peer))
}

/// A read or write error that means the port went away: the session ends with its summary.
fn gone(err: &io::Error) -> bool {
    matches!(err.kind(), io::ErrorKind::BrokenPipe | io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::NotConnected)
}

fn run(
    line: &mut impl Line,
    reset: Reset,
    limit: Option<Duration>,
    output: &mut dyn Write,
    mut stopped: impl FnMut() -> bool,
    mut peer: Option<&mut dyn Peer>,
) -> io::Result<()> {
    use Step::*;
    let reset_at = line.now();
    match reset {
        Reset::Dtr => {
            line.sequence(&[Dtr(true), Wait(Duration::from_millis(50)), Dtr(false), Drain])
                .map_err(|(_, err)| err)?;
        }
        Reset::Usj => {
            line.sequence(&[
                Dtr(false), Rts(false), Rts(true), Wait(Duration::from_millis(100)), Rts(false),
            ]).map_err(|(_, err)| err)?;
        }
        Reset::None => {}
    }
    let start = line.now();
    let mut link = Link::new();
    let mut ticks = Unwrap::new();
    let mut synced = false;
    let mut first_byte = None;
    let mut lost = 0u64;
    let mut lost_saturated = false;
    // After a USJ reset the ESP32-S3 ROM prints its banner before the image runs: until the first Hello,
    // bytes are ROM text, printed as `rom` lines, and the Hello is the longest tail before a delimiter that
    // decodes as one (the image's first frame follows the ROM's last line with no delimiter between).
    let mut rom: Option<Vec<(u8, Instant)>> = matches!(reset, Reset::Usj).then(Vec::new);
    let result = loop {
        if stopped() {
            break Ok(());
        }
        let now = line.now().saturating_duration_since(start);
        if let Some(peer) = peer.as_deref_mut() {
            peer.turn(&mut link, now, None);
            let mut wire = Vec::new();
            link.poll_raw(now.as_millis() as u64, &mut |packet| wire.extend_from_slice(packet));
            if !wire.is_empty() {
                match line.write_all(&wire) {
                    Ok(()) => {}
                    Err(err) if gone(&err) => break Ok(()),
                    Err(err) => break Err(err),
                }
            }
        }
        let timeout = match limit {
            Some(limit) => {
                let elapsed = line.now().saturating_duration_since(start);
                if elapsed >= limit {
                    break Ok(());
                }
                (limit - elapsed).min(Duration::from_secs(1))
            }
            None => Duration::from_secs(1),
        };
        let timeout = if peer.is_some() { timeout.min(TURN) } else { timeout };
        let byte = match line.read_byte(timeout) {
            Ok(Some(byte)) => byte,
            Ok(None) => continue,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) if gone(&err) => break Ok(()),
            Err(err) => break Err(err),
        };
        let now = line.now().saturating_duration_since(start);
        if let Some(text) = &mut rom {
            if byte != 0 {
                text.push((byte, line.now()));
                // Lines print once they lie a whole frame behind the newest byte: a frame's own bytes can be
                // a newline's, so the last MAX_WIRE bytes always stay for the Hello search.
                let settled = text.len().saturating_sub(MAX_WIRE);
                let done = match text[..settled].iter().rposition(|&(b, _)| b == b'\n') {
                    Some(end) => end + 1,
                    None if settled >= ROM_LINE => settled,
                    None => 0,
                };
                if done > 0 {
                    print_rom(output, &text[..done])?;
                    text.drain(..done);
                }
                continue;
            }
            let Some(start) = hello_start(text) else {
                text.push((0, line.now()));
                continue;
            };
            print_rom(output, &text[..start])?;
            first_byte = Some(text[start].1);
            for &(byte, _) in &text[start..] { link.push_raw(now.as_millis() as u64, byte); }
            rom = None;
        }
        if byte != 0 && first_byte.is_none() {
            first_byte = Some(line.now());
        }
        let decoded = link.push_raw(now.as_millis() as u64, byte);
        let arrival = if byte == 0 { first_byte.take() } else { None };
        if let Some(Ok(frame)) = decoded {
            if matches!(frame.event, Event::Hello { .. }) {
                synced = true;
            }
            if !synced {
                continue;
            }
            let tick = ticks.tick(&frame);
            if let Event::Lost { count, saturated, .. } = frame.event {
                lost_saturated |= saturated || lost.checked_add(u64::from(count)).is_none();
                lost = lost.saturating_add(u64::from(count));
            }
            let reset_ms = arrival.unwrap_or_else(|| line.now())
                .saturating_duration_since(reset_at).as_millis();
            print_frame(output, &frame, tick, reset_ms)?;
            output.flush()?;
            if let Some(peer) = peer.as_deref_mut() { peer.turn(&mut link, now, Some(&frame)); }
        }
    };
    if let Some(text) = &rom { print_rom(output, text)?; }
    let reader = link.reader();
    writeln!(output,
        "summary frames={} bad_cobs={} bad_crc={} bad_length={} bad_kind={} junk={} gaps={} lost={} lost_saturated={}",
        reader.frames, reader.bad.cobs, reader.bad.crc, reader.bad.length,
        reader.bad.kind, reader.junk, reader.gaps, lost, u8::from(lost_saturated),
    )?;
    if let Some(peer) = peer.as_deref() { peer.summary(output)?; }
    output.flush()?;
    result
}

/// Where in `text` the longest tail that decodes as a Hello starts.
fn hello_start(text: &[(u8, Instant)]) -> Option<usize> {
    (text.len().saturating_sub(MAX_WIRE)..text.len()).find(|&start| {
        let mut wire = [0; MAX_WIRE];
        let wire = &mut wire[..text.len() - start];
        for (byte, &(b, _)) in wire.iter_mut().zip(&text[start..]) { *byte = b; }
        let mut body = [0; MAX_BODY];
        cobs_decode(wire, &mut body)
            .is_some_and(|len| matches!(decode(&body[..len]), Ok(Frame { event: Event::Hello { .. }, .. })))
    })
}

/// ROM text, a `rom` line per line (empty ones left out): printable ASCII as is, other bytes escaped,
/// line ends left out.
fn print_rom(output: &mut dyn Write, text: &[(u8, Instant)]) -> io::Result<()> {
    let bytes: Vec<u8> = text.iter().map(|&(byte, _)| byte).collect();
    for line in bytes.split(|&byte| byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() { continue; }
        write!(output, "rom ")?;
        for &byte in line {
            match byte {
                0x20..=0x7e => write!(output, "{}", byte as char)?,
                other => write!(output, "\\x{other:02x}")?,
            }
        }
        writeln!(output)?;
    }
    Ok(())
}

fn print_frame(output: &mut dyn Write, frame: &Frame, tick: u64, reset_ms: u128) -> io::Result<()> {
    write!(output, "{} {tick} ", frame.seq)?;
    match frame.event {
        Event::Hello { proto, ports_version, image, clock_hz, prescale, ring, tick_hi, mcusr, wdtcsr, level, generation } => {
            let level = level.name();
            writeln!(output,
                "hello proto={proto} ports_version={ports_version} image={image} clock_hz={clock_hz} prescale={prescale} ring={ring} tick_hi={tick_hi} mcusr={mcusr} wdtcsr={wdtcsr} level={level} generation={generation} reset_ms={reset_ms}",
            )
        }
        Event::Write { pin, level, site } => writeln!(output, "write pin={pin} level={} site={site}", u8::from(level)),
        Event::Duty { pin, level, site } => writeln!(output, "duty pin={pin} level={level} site={site}"),
        Event::Read { pin, level, site } => writeln!(output, "read pin={pin} level={} site={site}", u8::from(level)),
        Event::Fault { code, site, arg } => {
            let name = match code {
                FAULT_PIN => "pin",
                FAULT_TIMER => "timer",
                FAULT_OVERRUN => "overrun",
                FAULT_LOOP_CAP => "loop_cap",
                FAULT_SERIAL => "serial",
                FAULT_SERIAL_LOST => "serial_lost",
                FAULT_SERIAL_LOST_AT_LEAST => "serial_lost_at_least",
                FAULT_WATCHDOG => "watchdog",
                FAULT_DRAW => "draw",
                FAULT_PANEL => "panel",
                FAULT_RANGE => "range",
                FAULT_SHIFT_BITS => "shift_bits",
                FAULT_I2C_NACK => "i2c_nack",
                FAULT_I2C_TIMEOUT => "i2c_timeout",
                FAULT_EEPROM_ADDR => "eeprom_addr",
                FAULT_KERNEL_BLOCK_IN_CRITICAL => "kernel_block_in_critical",
                FAULT_KERNEL_TIMER_REFUSED => "kernel_timer_refused",
                FAULT_KERNEL_CRIT_MISMATCH => "kernel_crit_mismatch",
                FAULT_KERNEL_CRIT_SPAN => "kernel_crit_span",
                FAULT_KERNEL_ISR_DEPTH => "kernel_isr_depth",
                FAULT_RADIO_PHY_INIT_FAILED => "radio_phy_init_failed",
                FAULT_RADIO_PHY_CAL_STORE_FAILED => "radio_phy_cal_store_failed",
                FAULT_KERNEL_IRQ_LINE_REFUSED => "kernel_irq_line_refused",
                FAULT_KERNEL_IRQ_SOURCE_REFUSED => "kernel_irq_source_refused",
                FAULT_KERNEL_ISR_TABLE_FULL => "kernel_isr_table_full",
                FAULT_KERNEL_ISR_ENTRY_REFUSED => "kernel_isr_entry_refused",
                FAULT_RADIO_ISLAND_MISMATCH => "radio_island_mismatch",
                FAULT_KERNEL_TIMER_LOST => "kernel_timer_lost",
                FAULT_KERNEL_TIMER_UNSET => "kernel_timer_unset",
                FAULT_KERNEL_HEAP_EXHAUSTED => "kernel_heap_exhausted",
                FAULT_KERNEL_TIMER_HANDLE_INVALID => "kernel_timer_handle_invalid",
                FAULT_RADIO_STARTED => "radio_started",
                FAULT_RADIO_PHY_TABLE_REFUSED => "radio_phy_table_refused",
                FAULT_RADIO_EVENT => "radio_event",
                FAULT_RADIO_START_FAILED => "radio_start_failed",
                _ => "unknown",
            };
            writeln!(output, "fault {name} code={code} arg={arg} site={site}")
        }
        Event::Lost { count, saturated, first_tick } => {
            // first_tick is at or before the frame tick, within one low-word
            // wrap. Wrapping subtraction also handles the 48-bit clock wrap.
            let elapsed = u64::from(frame.tick.wrapping_sub(first_tick));
            let first = tick.wrapping_sub(elapsed) & 0x0000_ffff_ffff_ffff;
            writeln!(output, "lost count={count} first={first} last={tick} saturated={}", u8::from(saturated))
        }
        Event::Time { tick_hi } => writeln!(output, "time tick_hi={tick_hi}"),
        Event::Probe { id, value } => writeln!(output, "probe id={id} value={value}"),
        Event::Rx { byte } => writeln!(output, "rx byte={byte}"),
        Event::Eth { len } => writeln!(output, "eth len={len}"),
        // the kind and length only: a message's bytes are the program's, never the monitor log's
        Event::Net { len } => {
            let kind = match frame.kind { NET_OPEN => "open", NET_CLOSE => "close", NET_DATA => "data", _ => "credit" };
            writeln!(output, "net {kind} len={len}")
        }
        Event::EthCredit { count } => writeln!(output, "eth_credit count={count}"),
        Event::Link { counter, value } => writeln!(output, "link counter={counter} value={value}"),
        Event::Shift { data, clock, value, bits, site } => writeln!(output, "shift data={data} clock={clock} value=0x{value:08x} bits={bits} site={site}"),
        Event::EepromRead { addr, value, site } => writeln!(output, "eeprom_read addr={addr} value={value} site={site}"),
        Event::EepromWrite { addr, value, site } => writeln!(output, "eeprom_write addr={addr} value={value} site={site}"),
        Event::RadioRecord { index, words } => writeln!(output, "radio_record index={index} words={words:?}"),
    }
}

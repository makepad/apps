//! Measures Ethernet throughput over the USB tether with `fixtures/tether_rate.fw.splash`.

use crate::{tether::{ADDRESSES, AHEAD, ETHERTYPE}, watch::Peer};
use fab_elec_mcu_netgw::{Link, TetherStats};
use fab_elec_mcu_rt::watch::{Event, Frame, ETH_MAX};
use std::{io::{self, Write}, time::Duration};

const SETTLE: Duration = Duration::from_secs(1);

fn header(kind: u8) -> [u8; 15] {
    let mut bytes = [0; 15];
    bytes[..12].copy_from_slice(&ADDRESSES);
    bytes[12..14].copy_from_slice(&ETHERTYPE.to_be_bytes());
    bytes[14] = kind;
    bytes
}

#[derive(Clone, Copy)]
enum Phase { Down, Up, Both }

#[derive(Clone, Copy)]
struct Mark { at: Duration, sent: u64, received: u64 }

#[derive(Clone, Copy, Default)]
struct Window { settled: Option<Mark>, end: Option<Mark> }

/// Measures down, up, then both together. A fresh Hello starts all three phases again.
pub struct Rate {
    phase_time: Duration,
    active: Option<(Phase, Duration)>,
    windows: [Window; 3],
    data: [u8; ETH_MAX],
    /// Position of the U frame in the link's cumulative send count.
    up_command: Option<u64>,
    device_frames: [u64; 2],
    stats: TetherStats,
    credit_high_water: u32,
}

impl Rate {
    pub fn new(phase: Duration) -> Self {
        let mut data = [0; ETH_MAX];
        data[..15].copy_from_slice(&header(b'D'));
        Self {
            phase_time: phase, active: None, windows: [Window::default(); 3], data,
            up_command: None, device_frames: [0; 2], stats: TetherStats::default(), credit_high_water: 0,
        }
    }
}

impl Peer for Rate {
    fn turn(&mut self, link: &mut Link, now: Duration, frame: Option<&Frame>) {
        match frame.map(|f| f.event) {
            Some(Event::Hello { .. }) => {
                self.active = Some((Phase::Down, now));
                self.windows = [Window::default(); 3];
                self.up_command = None;
                self.device_frames = [0; 2];
            }
            Some(Event::Probe { id, value }) if (1..=2).contains(&id) => {
                let total = &mut self.device_frames[usize::from(id - 1)];
                *total += u64::from(value.wrapping_sub(*total as u16));
            }
            _ => {}
        }

        let stats = link.stats().tether;
        self.credit_high_water = self.credit_high_water.max(stats.credits);
        let mark = Mark { at: now, sent: stats.frames_sent, received: stats.frames_in };
        while let Some((phase, start)) = self.active {
            let elapsed = now - start;
            let window = &mut self.windows[phase as usize];
            if elapsed < self.phase_time {
                if elapsed >= SETTLE && window.settled.is_none() { window.settled = Some(mark); }
                if matches!(phase, Phase::Down | Phase::Both) {
                    for _ in link.stats().tether.queue_len..AHEAD {
                        link.queue_frame(&self.data).expect("the rate data frame fits Ethernet");
                    }
                }
                break;
            }

            window.end = Some(mark);
            let next_start = start + self.phase_time;
            self.active = match phase {
                Phase::Down => {
                    // The FIFO can still hold D frames. Remember which send is U in case
                    // credits arrive so late that it falls inside the bidirectional window.
                    self.up_command = Some(stats.frames_sent + stats.queue_len as u64 + 1);
                    link.queue_frame(&header(b'U')).expect("the rate command fits Ethernet");
                    Some((Phase::Up, next_start))
                }
                Phase::Up => Some((Phase::Both, next_start)),
                Phase::Both => {
                    link.queue_frame(&header(b'X')).expect("the rate command fits Ethernet");
                    None
                }
            };
        }
        self.stats = link.stats().tether;
    }

    fn summary(&self, output: &mut dyn Write) -> io::Result<()> {
        for (phase, name) in [(Phase::Down, "down"), (Phase::Up, "up"), (Phase::Both, "both")] {
            write!(output, "tether rate {name}: ")?;
            let window = self.windows[phase as usize];
            let Some(end) = window.end else { writeln!(output, "incomplete")?; continue };
            let Some(start) = window.settled else {
                writeln!(output, "no measurement window after 1 s settle")?;
                continue;
            };
            let secs = (end.at - start.at).as_secs_f64();
            let control = self.up_command.is_some_and(|n| start.sent < n && n <= end.sent);
            let down = end.sent - start.sent - u64::from(control);
            let up = end.received - start.received;
            let kb_s = |frames: u64| frames as f64 * ETH_MAX as f64 / 1000.0 / secs;
            match phase {
                Phase::Down => writeln!(output, "{:.1} kB/s ({down} frames in {secs:.2} s)", kb_s(down))?,
                Phase::Up => writeln!(output, "{:.1} kB/s ({up} frames in {secs:.2} s)", kb_s(up))?,
                Phase::Both => writeln!(output, "down {:.1} kB/s, up {:.1} kB/s ({down} down, {up} up frames in {secs:.2} s)", kb_s(down), kb_s(up))?,
            }
        }
        writeln!(output, "tether rate restarts={} credits_received={} credit_high_water={} queue_high_water={} probe_down_frames={} probe_up_frames={}",
            self.stats.hellos.saturating_sub(1), self.stats.credits_received, self.credit_high_water,
            self.stats.queue_high_water, self.device_frames[0], self.device_frames[1])
    }
}

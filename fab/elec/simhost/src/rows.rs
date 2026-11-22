//! The host's net, segment, part and pin textures follow the `live-rows.md` texel contract.
//! Shared live helpers pack every value; segment phases survive current changes and clock rebases.
//! Pin rows show a co-simulated device's published state (`LiveValues::co`) through the app's
//! [`CoPinReader`]; they are written only when that state is a new `Arc`.

use fab_elec_sim::cosim::CoState;
use fab_elec_view::live::{
    current_level, emission_attr, glow_attr, light_moved, net_row, pin_row, FlowPhase, LiveClock, LiveLook,
    LiveRows, PinLiveMode, NET_OFF, PIN_OFF,
};
use makepad_widgets_core::{Cx, Texture};
use std::collections::HashMap;
use std::sync::Arc;

/// Reads a co-simulated device's pins out of its published state (`LiveValues::co`): each pin by
/// its pin table name (`CoSimPin::name`), with its drawing mode, its level (None: unknown) and its
/// PWM duty. The app that makes the device knows its state's type; the host does not.
pub type CoPinReader = Arc<dyn Fn(&CoState, &mut dyn FnMut(&str, PinLiveMode, Option<bool>, f32)) + Send + Sync>;

/// An LED's part index, the pin its forward current enters (the anode), and rated amperes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Emitter {
    pub part: u32,
    pub pin: u32,
    pub i_rated: f32,
}

/// A lowering's pin nets (`ValuePlan::pin_net`), LEDs and co-simulated pins: each pin table name
/// to the pin index of the pad its GPIO drives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowPlan {
    pub pin_net: Vec<Option<u32>>,
    pub emitters: Vec<Emitter>,
    /// Full-glow watts by part index. Missing entries use the nominal fallback.
    pub p_rated: Vec<f32>,
    pub co_pins: HashMap<String, u32>,
}

/// Values by plan index: interval means live, cursor samples while scrubbing.
/// `pin_i` is the current into each pin; `led_i` the one an LED's light reads (the means while the
/// run goes on, the sample at `t` when it stands).
#[derive(Clone, Copy, Debug)]
pub struct RowValues<'a> {
    pub net_v: &'a [f32],
    pub pin_i: &'a [f32],
    pub led_i: &'a [f32],
    pub part_p: &'a [f32],
}

/// The four textures views bind, with the host's emphasis and continuous flow clock.
pub struct LiveTextures {
    nets: LiveRows,
    segs: LiveRows,
    parts: LiveRows,
    pins: LiveRows,
    pin_emphasis: Vec<f32>,
    /// The device state the pin rows show (None: none written) and the rows it wrote.
    pins_from: Option<CoState>,
    pins_written: Vec<u32>,
    flows: Vec<(FlowPhase, f32)>,
    clock: LiveClock,
    net_emphasis: Vec<f32>,
    part_emphasis: Vec<f32>,
    // Reused for the one-pass sum over pins; writes allocate nothing after resize.
    net_through: Vec<f32>,
    part_emission: Vec<f32>,
    /// The brightest LED light output the part rows hold (their y, 0..1).
    emission: f32,
    /// Moves when any part row's LED light output (y) changes so it can show (`light_moved`
    /// against `lights_shown`) or the rows renumber.
    lights: u64,
    /// Each part row's light output when `lights` last moved: a slow drift adds up against it.
    lights_shown: Vec<f32>,
    look: LiveLook,
}

impl LiveTextures {
    /// No rows yet; the clock's epoch is `now` (frame time, seconds).
    pub fn new(now: f64, look: LiveLook) -> LiveTextures {
        LiveTextures {
            nets: LiveRows::new(),
            segs: LiveRows::new(),
            parts: LiveRows::new(),
            pins: LiveRows::with_off(PIN_OFF),
            pin_emphasis: Vec::new(),
            pins_from: None,
            pins_written: Vec::new(),
            flows: Vec::new(),
            clock: LiveClock::new(now),
            net_emphasis: Vec::new(),
            part_emphasis: Vec::new(),
            net_through: Vec::new(),
            part_emission: Vec::new(),
            emission: 0.0,
            lights: 0,
            lights_shown: Vec::new(),
            look,
        }
    }

    pub fn look(&self) -> &LiveLook {
        &self.look
    }

    /// A new look (theme), used from the next values write on.
    pub fn set_look(&mut self, look: LiveLook) {
        self.look = look;
    }

    /// Size the lowering's net and part rows and their emphasis.
    pub fn resize(&mut self, nets: usize, parts: usize) {
        let old_parts = self.parts.nets();
        self.nets.resize(nets);
        self.parts.resize(parts);
        self.net_emphasis.resize(nets, 0.0);
        self.part_emphasis.resize(parts, 0.0);
        self.net_through.resize(nets, 0.0);
        self.part_emission.resize(parts, 0.0);
        for part in old_parts..parts {
            self.parts.set(part, [0.0; 4]);
        }
        self.lights_shown.resize(parts, 0.0);
        self.emission = (0..parts).map(|part| self.parts.row(part)[1]).fold(0.0, f32::max);
        // a new lowering renumbers the rows: the light a row holds now belongs to another part
        self.show_lights();
    }

    /// The lights move: what each part row holds now is what views show.
    fn show_lights(&mut self) {
        self.lights += 1;
        for (part, shown) in self.lights_shown.iter_mut().enumerate() {
            *shown = self.parts.row(part)[1];
        }
    }

    /// Size the lowering's pin rows: every row goes off (a new lowering renumbers them) and the next
    /// `write_pins` writes its state whatever it is.
    pub fn resize_pins(&mut self, pins: usize) {
        self.pins.resize(pins);
        self.pin_emphasis.clear();
        self.pin_emphasis.resize(pins, 0.0);
        self.clear_pins();
    }

    /// Pin rows from a device's published state when it is not the one they show (a pointer
    /// compare): rows that had state and have none now go off, each pin the reader names that the
    /// plan has gets its row. Emphasis is kept. True when it wrote.
    pub fn write_pins(&mut self, plan: &RowPlan, co: Option<&CoState>, reader: Option<&CoPinReader>) -> bool {
        if self.pins_from.as_ref() == co {
            return false;
        }
        self.pins_from = co.cloned();
        for row in std::mem::take(&mut self.pins_written) {
            let emphasis = self.pin_emphasis.get(row as usize).copied().unwrap_or(0.0);
            self.pins.set(row as usize, pin_row(None, None, 0.0, emphasis));
        }
        if let (Some(co), Some(reader)) = (co, reader) {
            let (pins, emphasis, written) = (&mut self.pins, &self.pin_emphasis, &mut self.pins_written);
            reader(co, &mut |name: &str, mode: PinLiveMode, level: Option<bool>, duty: f32| {
                if let Some(&row) = plan.co_pins.get(name).filter(|&&row| (row as usize) < pins.nets()) {
                    let e = emphasis.get(row as usize).copied().unwrap_or(0.0);
                    pins.set(row as usize, pin_row(Some(mode), level, duty, e));
                    written.push(row);
                }
            });
        }
        true
    }

    /// Every pin row off (a scrub shows another instant, or the state's reader changed); the next
    /// `write_pins` writes whatever state it gets.
    pub fn clear_pins(&mut self) {
        for (row, &emphasis) in self.pin_emphasis.iter().enumerate() {
            self.pins.set(row, pin_row(None, None, 0.0, emphasis));
        }
        self.pins_from = None;
        self.pins_written.clear();
    }

    /// Emphasise only these pin indices, updating the stored texels immediately.
    pub fn emphasis_pins(&mut self, pins: &[u32]) {
        self.pin_emphasis.fill(0.0);
        for &pin in pins {
            if let Some(emphasis) = self.pin_emphasis.get_mut(pin as usize) {
                *emphasis = 1.0;
            }
        }
        for (pin, &emphasis) in self.pin_emphasis.iter().enumerate() {
            let mut row = self.pins.row(pin);
            row[3] = emphasis;
            self.pins.set(pin, row);
        }
    }

    /// Grow segment storage; existing rows never move or shrink.
    pub fn resize_segs(&mut self, rows: usize) {
        let old_rows = self.segs.nets();
        if rows <= old_rows {
            return;
        }
        self.segs.resize(rows);
        self.flows.resize(rows, (FlowPhase::default(), 0.0));
        for row in old_rows..rows {
            self.segs.set(row, [0.0; 4]);
        }
    }

    /// Write nets and parts using the shared attribute helpers, leaving absent values untouched.
    pub fn write_values(&mut self, plan: &RowPlan, v: &RowValues<'_>) {
        self.net_through.fill(0.0);
        for (net, &current) in plan.pin_net.iter().zip(v.pin_i) {
            if let Some(net) = net {
                if let Some(sum) = self.net_through.get_mut(*net as usize) {
                    *sum += current.abs();
                }
            }
        }
        for (net, ((&voltage, &sum), &emphasis)) in v.net_v.iter()
            .zip(&self.net_through).zip(&self.net_emphasis).enumerate()
        {
            let voltage = voltage.is_finite().then_some(voltage);
            self.nets.set(net, net_row(voltage, 0.5 * sum, emphasis, &self.look));
        }
        self.part_emission.fill(0.0);
        for emitter in &plan.emitters {
            if let (Some(emission), Some(&current)) = (
                self.part_emission.get_mut(emitter.part as usize),
                v.led_i.get(emitter.pin as usize),
            ) {
                *emission = emission_attr(current, emitter.i_rated);
            }
        }
        self.emission = 0.0;
        let mut lit_changed = false;
        for (part, (((&power, &emphasis), &emission), &shown)) in v.part_p.iter()
            .zip(&self.part_emphasis).zip(&self.part_emission).zip(&self.lights_shown).enumerate()
        {
            lit_changed |= light_moved(shown, emission);
            let rating = plan.p_rated.get(part).copied().unwrap_or(fab_elec_bench::lower::DEFAULT_P_RATED);
            self.parts.set(part, [glow_attr(power, rating), emission, power, emphasis]);
            self.emission = self.emission.max(emission);
        }
        if lit_changed { self.show_lights(); }
    }

    /// Write currents along each segment's first-to-second-node direction, preserving phase.
    /// Out-of-range rows and unmatched slice tails are skipped.
    pub fn write_segs(&mut self, now: f64, rows: &[u32], i: &[f32]) {
        for (&row, &current) in rows.iter().zip(i) {
            let row = row as usize;
            if let Some((phase, last_current)) = self.flows.get_mut(row) {
                *last_current = current;
                phase.set(now, *last_current, &self.look);
                let [offset, speed] = phase.attr(&self.clock);
                self.segs.set(row, [offset, speed, current_level(*last_current, &self.look), 0.0]);
            }
        }
    }

    /// Zero freed rows and reset their flow phase and remembered current.
    pub fn clear_segs(&mut self, rows: &[u32]) {
        for &row in rows {
            let row = row as usize;
            if let Some(flow) = self.flows.get_mut(row) {
                *flow = (FlowPhase::default(), 0.0);
                self.segs.set(row, [0.0; 4]);
            }
        }
    }

    /// Turn live values off, retaining net, part and pin emphasis and resetting every flow.
    pub fn clear(&mut self) {
        self.clear_pins();
        for (net, &emphasis) in self.net_emphasis.iter().enumerate() {
            let mut row = NET_OFF;
            row[3] = emphasis;
            self.nets.set(net, row);
        }
        let mut lit_changed = false;
        for (part, (&emphasis, &shown)) in self.part_emphasis.iter().zip(&self.lights_shown).enumerate() {
            lit_changed |= light_moved(shown, 0.0);
            self.parts.set(part, [0.0, 0.0, 0.0, emphasis]);
        }
        if lit_changed { self.show_lights(); }
        self.emission = 0.0;
        self.flows.fill((FlowPhase::default(), 0.0));
        for row in 0..self.segs.nets() {
            self.segs.set(row, [0.0; 4]);
        }
    }

    /// Emphasise only these net and part indices, updating the stored texels immediately.
    pub fn emphasis(&mut self, nets: &[u32], parts: &[u32]) {
        self.net_emphasis.fill(0.0);
        self.part_emphasis.fill(0.0);
        for &net in nets {
            if let Some(emphasis) = self.net_emphasis.get_mut(net as usize) {
                *emphasis = 1.0;
            }
        }
        for &part in parts {
            if let Some(emphasis) = self.part_emphasis.get_mut(part as usize) {
                *emphasis = 1.0;
            }
        }
        for (net, &emphasis) in self.net_emphasis.iter().enumerate() {
            let mut row = self.nets.row(net);
            row[3] = emphasis;
            self.nets.set(net, row);
        }
        for (part, &emphasis) in self.part_emphasis.iter().enumerate() {
            let mut row = self.parts.row(part);
            row[3] = emphasis;
            self.parts.set(part, row);
        }
    }

    /// Shader time for this frame; rebase every segment's phase when the clock epoch moves.
    pub fn live_time(&mut self, now: f64) -> f32 {
        let (time, moved) = self.clock.time(now);
        if moved {
            for (row, (phase, _)) in self.flows.iter().enumerate() {
                let [offset, speed] = phase.attr(&self.clock);
                // An epoch move preserves the level from the last values write, including
                // when a new look is waiting for that next write.
                let level = self.segs.row(row)[2];
                self.segs.set(row, [offset, speed, level, 0.0]);
            }
        }
        time
    }

    /// Upload changed rows once per frame and return the net, segment, part and pin textures.
    pub fn upload(&mut self, cx: &mut Cx) -> [Texture; 4] {
        [self.nets.texture(cx).clone(), self.segs.texture(cx).clone(), self.parts.texture(cx).clone(), self.pins.texture(cx).clone()]
    }

    pub fn nets(&self) -> &LiveRows {
        &self.nets
    }

    pub fn segs(&self) -> &LiveRows {
        &self.segs
    }

    pub fn parts(&self) -> &LiveRows {
        &self.parts
    }

    /// The brightest LED light output the part rows hold (their y, 0..1; [`fab_elec_view::live::sim::SimLive::emission`]).
    pub fn emission(&self) -> f32 {
        self.emission
    }

    /// Moves whenever a part row's LED light output (y) changes so it can show
    /// ([`fab_elec_view::live::light_moved`]), or the rows renumber
    /// ([`fab_elec_view::live::sim::SimLive::lights`]); holds while every light looks the same.
    pub fn lights(&self) -> u64 {
        self.lights
    }

    pub fn pins(&self) -> &LiveRows {
        &self.pins
    }
}

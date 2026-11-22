//! The Mechanics Closed scene runs the device's board. The bar controls its bench and Listen
//! adds the case's output jack (or a voltage probe) to the app's heard set.

use crate::device_assembly::DeviceAssembly;
use fab::api::{AppState, MechScene, PaneId, PanePoses, PlateMode, PoseSetId};
use fab_elec_bench::SimCmd;
use fab_elec_core::CompRef;
use fab_elec_design::ProbeQuantity;
use fab_elec_mech::CaseBinding;
use fab_elec_view::live::listen::{listen, listen_jack, listen_status, listening, listening_jack, record, ListenMode};
use fab_elec_view::live::sim::SimAction;
use fab_elec_view::ElecLive;
use makepad_widgets::*;

/// What this box's Listen added to the app's heard set: the board's output jack, or a probed net.
#[derive(Clone, Debug, PartialEq)]
enum Heard {
    Jack(CompRef),
    Net(String),
}

#[derive(Default)]
pub struct ClosedBox {
    shown: bool,
    /// What this box's Listen added to the app's heard set.
    heard: Option<Heard>,
    /// Listen was pressed before the board started or its case closed: it is heard once they have.
    listen_waits: bool,
    /// What the status line says: keys, knobs and pipes, playing, the speed (in thousandths), the
    /// keys down and a hint.
    said: Option<([usize; 3], bool, i64, usize, &'static str)>,
}

impl ClosedBox {
    pub fn showing(state: &AppState) -> bool {
        state.ui.in_mode(PlateMode::Mechanics) && state.mech.scene == MechScene::Closed
    }

    /// Keep the lead Plate view and the views of its camera (an Underside view) on the device
    /// while closed, including one opened mid-scene. O(panes).
    pub fn sync_panes(state: &mut AppState) -> bool {
        let closed = Self::showing(state);
        let device = PanePoses::Set(PoseSetId::DEVICE);
        if closed || state.pane_poses(state.view.lead_plate()) == device {
            let poses = if closed { device } else { PanePoses::Stage };
            let lead = state.view.lead_plate();
            state.set_pane_poses(lead, poses);
            for i in 0..state.view.panes.len() {
                let pane = PaneId(i as u16);
                if state.view.same_camera(lead, pane) {
                    state.set_pane_poses(pane, poses);
                }
            }
        }
        closed
    }

    pub fn handle_event(&mut self, cx: &mut Cx, state: &mut AppState, has_plate: bool, assembly: &DeviceAssembly) {
        if !Self::showing(state) {
            if self.shown || self.listen_waits || self.said.is_some() {
                self.hide(cx, state, assembly);
            }
            return;
        }
        if !cx.has_global::<ElecLive>() {
            if state.mech.listening != self.listen_waits {
                state.mech.listening = self.listen_waits;
                cx.redraw_all();
            }
            self.say(state, [0, 0, 0], false, 0, 0, "Starting the board…");
            return;
        }
        if !self.shown {
            self.shown = true;
            // the box runs its board
            if !cx.get_global::<ElecLive>().sim.playing {
                cx.action(SimAction::Cmd(SimCmd::Play));
            }
        }
        if self.listen_waits && !assembly.resolving() {
            self.listen_waits = false;
            self.set_listen(cx, true, assembly);
        }
        // a Scope chip may stop hearing the box's output: the toggle follows the app's set
        let still = |heard: &Heard| match heard {
            Heard::Jack(jack) => listening_jack(cx, *jack),
            Heard::Net(net) => listening(cx, net),
        };
        if self.heard.as_ref().is_some_and(|heard| !still(heard)) {
            self.heard = None;
        }
        // what the output plays, and how far the stretch it renders (or plays round) has come
        let heard = self.heard.as_ref().and_then(|_| listen_status(cx));
        let line = heard.as_ref().map(|st| st.to_string()).unwrap_or_default();
        let progress = heard.and_then(|st| match st.mode {
            ListenMode::Rendering { done_s, length_s, .. } if length_s > 0.0 => Some((done_s / length_s).clamp(0.0, 1.0)),
            ListenMode::Playing { length_s, ahead_s } if length_s > 0.0 => Some((ahead_s / length_s).clamp(0.0, 1.0)),
            _ => None,
        });
        if state.mech.listen_line != line || state.mech.listen_progress != progress {
            (state.mech.listen_line, state.mech.listen_progress) = (line, progress);
            cx.redraw_all();
        }
        let live = cx.get_global::<ElecLive>();
        let (playing, status) = (live.sim.playing, live.sim.status.clone());
        let speed = status.rate_achieved.filter(|_| playing).map_or(0, |r| (r * 1000.0).round() as i64);
        let held = &live.sim.held;
        let case = assembly.case();
        let (mut keys, mut knobs, mut pipes, mut down) = (0, 0, 0, 0);
        if let Some(case) = case {
            for binding in case.bindings() {
                match binding {
                    CaseBinding::Key { comp, .. } => {
                        keys += 1;
                        down += usize::from(held.contains(&comp));
                    }
                    CaseBinding::Knob { .. } => knobs += 1,
                    CaseBinding::Pipe { .. } => pipes += 1,
                    CaseBinding::None => {}
                }
            }
        }
        let hint = match case {
            None if !has_plate => "Nothing on the plate yet",
            None if assembly.resolving() => "Closing the case…",
            None => "No board's case on this plate",
            Some(_) if !playing => "Play runs the board",
            Some(_) if knobs == 0 => "Press a key",
            Some(_) if keys == 0 => "Drag a knob up or down to turn it",
            Some(_) => "Press a key, or drag a knob up or down",
        };
        let on = self.heard.is_some() || self.listen_waits;
        if state.mech.running != playing || state.mech.listening != on || state.mech.speed != status.rate_asked {
            (state.mech.running, state.mech.listening, state.mech.speed) = (playing, on, status.rate_asked);
            cx.redraw_all();
        }
        self.say(state, [keys, knobs, pipes], playing, speed, down, hint);
    }

    /// The bar controls playback, speed and Listen on the closed box's board.
    pub fn action(&mut self, cx: &mut Cx, a: &fab::api::MechAction, assembly: &DeviceAssembly) -> bool {
        use fab::api::MechAction;
        match a {
            MechAction::Play => cx.action(SimAction::Cmd(SimCmd::Play)),
            MechAction::Pause => cx.action(SimAction::Cmd(SimCmd::Pause)),
            MechAction::Speed(rate) => cx.action(SimAction::Cmd(SimCmd::SetRate(*rate))),
            MechAction::Listen(on) => self.set_listen(cx, *on, assembly),
            _ => return false,
        }
        cx.new_next_frame();
        true
    }

    fn hide(&mut self, cx: &mut Cx, state: &mut AppState, assembly: &DeviceAssembly) {
        self.shown = false;
        self.said = None;
        self.set_listen(cx, false, assembly);
        state.mech.listening = false;
        state.mech.listen_line.clear();
        state.mech.listen_progress = None;
    }

    /// Listen on: the board's output jack joins the app's heard set (else the running bench's
    /// first voltage probe); off: what this box added leaves it.
    fn set_listen(&mut self, cx: &mut Cx, on: bool, assembly: &DeviceAssembly) {
        // before the board's first snapshot (or while its case closes) there is nothing to hear yet:
        // the press waits for them, so what it hears is the case's jack, not a stand-in
        if on && (!cx.has_global::<ElecLive>() || !self.shown || assembly.resolving()) {
            self.listen_waits = true;
            return;
        }
        self.listen_waits = false;
        if on == self.heard.is_some() {
            return;
        }
        match self.heard.take() {
            Some(Heard::Jack(jack)) => return listen_jack(cx, jack, false),
            Some(Heard::Net(net)) => return listen(cx, &net, false),
            None => {}
        }
        let output = assembly.case().and_then(|case| case.output);
        let (probe, window) = {
            let bench = cx.get_global::<ElecLive>().run_bench();
            (bench.and_then(|b| b.out.probes.iter().find(|p| p.of.quantity == ProbeQuantity::V)).map(|p| p.of.target.clone()),
             bench.and_then(|b| b.out.stop))
        };
        match output.map(Heard::Jack).or(probe.map(Heard::Net)) {
            Some(heard) => {
                // below real time it is heard in windows of one run of the board's bench (its
                // `run: {stop}`: a pass of the pattern where the bench is written so), else the
                // listen default
                if let Some(seconds) = window {
                    record(cx, seconds);
                }
                match &heard {
                    Heard::Jack(jack) => listen_jack(cx, *jack, true),
                    Heard::Net(net) => listen(cx, net, true),
                }
                self.heard = Some(heard);
            }
            None => log!("closed box: the board has no audio output part and its bench probes no voltage to listen to"),
        }
    }

    /// The Mechanics status line, written only when what it says changes.
    fn say(&mut self, state: &mut AppState, bound: [usize; 3], playing: bool, speed: i64, down: usize, hint: &'static str) {
        let said = (bound, playing, speed, down, hint);
        if self.said == Some(said) {
            return;
        }
        self.said = Some(said);
        let pace = match (playing, speed) {
            (false, _) => String::new(),
            (true, s) if s >= 980 => " · real time".into(),
            (true, s) => format!(" · {:.3}× real time", s as f64 / 1000.0),
        };
        let hint = match down {
            0 => hint.to_string(),
            1 => "1 key down".to_string(),
            n => format!("{n} keys down"),
        };
        let [keys, knobs, pipes] = bound;
        let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let mut parts = vec![count(keys, "key", "keys")];
        if knobs > 0 {
            parts.push(count(knobs, "knob", "knobs"));
        }
        parts.push(count(pipes, "light pipe", "light pipes"));
        state.mech.status = if keys + knobs + pipes == 0 { hint } else { format!("Closed box: {}{pace} · {hint}", parts.join(", ")) };
    }
}

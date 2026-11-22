//! Source identity, coverage and capability sheets beside the pinned scope look controls.

use super::{
    axis::si_label,
    look::InstrumentLook,
    phosphor::grade_x,
    scope::{LabelCache, Persistence, INTENSITY_MAX, INTENSITY_MIN},
    trigger_ui::{afterglow_at, afterglow_position},
};
use makepad_widgets_core::*;
use fab_elec_bench::source::{Atten, Caps, ChannelCaps, ChannelInfo, ChanKind, Coupling, Depth, Mode, Modes, Over, Powers, Ranges, RateCaps, Resolved, Side, SourceInfo, State, Status, TriggerCaps, TriggerKind, TrigResolved};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SeeAs { #[default] Exact, BenchScope }
impl SeeAs {
    fn label(self) -> &'static str { match self { Self::Exact => "exact", Self::BenchScope => "bench scope" } }
    fn other(self) -> Self { match self { Self::Exact => Self::BenchScope, Self::BenchScope => Self::Exact } }
}

/// State words are shown only when the source is not live.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SourceState { #[default] Live, Paused, Waiting, Stale, Gone, Compiling, Unsimulated }
impl SourceState {
    fn word(self) -> &'static str {
        match self {
            Self::Live => "", Self::Paused => "paused", Self::Waiting => "waiting", Self::Stale => "stale", Self::Gone => "gone",
            // The run waits for its first firmware image: the circuit alone settles meanwhile.
            Self::Compiling => "compiling firmware…",
            // a probe names a net the run does not solve (the chip's note names it)
            Self::Unsimulated => "net not simulated",
        }
    }
    pub fn from_status(status: &Status) -> Self {
        match status.state {
            State::Paused => Self::Paused, State::Gone => Self::Gone,
            State::Armed if status.sample_rate > 0.0 => Self::Live,
            State::Opening | State::Idle | State::Armed | State::Waiting => Self::Waiting,
            _ if status.sample_rate == 0.0 => Self::Stale,
            _ => Self::Live,
        }
    }
}

/// A retained source snapshot. Caps are shared from open; resolved changes only on setup changes.
#[derive(Clone, Debug, PartialEq)]
pub struct SourceChip {
    pub info: SourceInfo, pub state: SourceState, pub caps: Arc<Caps>,
    pub resolved: Option<Arc<Resolved>>, pub trigger_kind: TriggerKind,
    /// What the state means here, in words, shown in its place (`1 probe: net not simulated (I2C
    /// clock)`); None shows the state's word.
    pub note: Option<String>,
}

impl SourceChip {
    fn state_text(&self) -> &str {
        self.note.as_deref().unwrap_or(self.state.word())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Coverage { All { triggers: f64 }, Dropped { blind: f64 }, Blocks { captures: f64, blind: f64 } }
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answers { pub coverage: Option<Coverage>, pub sim_ratio: Option<f64> }
impl Answers {
    /// Full and compact answers from one wall-second's feed totals, never sample records.
    pub fn from_stats(now: super::feed::FeedStats, previous: super::feed::FeedStats, seconds: f64, blind: Option<f64>, sim_ratio: Option<f64>) -> Self {
        let frames = now.frames.saturating_sub(previous.frames);
        let dropped = now.dropped.saturating_sub(previous.dropped);
        let lost = dropped as f64 / (frames + dropped).max(1) as f64;
        let rate = now.trigger_events.saturating_sub(previous.trigger_events) as f64 / seconds.max(f64::EPSILON);
        let coverage = if let Some(blind) = blind {
            Coverage::Blocks { captures: rate, blind: 1.0 - (1.0 - blind.clamp(0.0, 1.0)) * (1.0 - lost) }
        } else if dropped > 0 { Coverage::Dropped { blind: lost } } else { Coverage::All { triggers: rate } };
        Self { coverage: Some(coverage), sim_ratio }
    }
    pub fn text(&self, short: bool) -> Vec<String> {
        let mut text = Vec::new();
        if let Some(coverage) = self.coverage {
            text.push(match coverage {
                // Under one a second rounds to none: a steady or untriggered source says so instead of "all 0".
                Coverage::All { triggers } if triggers < 0.5 => "no triggers".into(),
                Coverage::All { triggers } => if short { "all drawn".into() } else { format!("all {} triggers/s drawn", grouped(triggers.round() as u64)) },
                Coverage::Dropped { blind } => format!("blind {:.0} %", blind * 100.0),
                Coverage::Blocks { captures, blind } => if short { format!("blind {:.1} %", blind * 100.0) }
                    else { format!("{} captures/s, blind {:.1} %", grouped(captures.round() as u64), blind * 100.0) },
            });
        }
        // The rate as a factor of real time, so a slow run reads as slow: "0.45× real time",
        // written as the transport writes it.
        if let Some(ratio) = self.sim_ratio.filter(|r| r.is_finite() && *r > 0.0) {
            let rate = crate::live::strip::format_reached(ratio);
            text.push(if short { format!("{rate}×") } else { format!("{rate}× real time") });
        }
        text
    }
}

fn grouped(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() { if i > 0 && (digits.len() - i) % 3 == 0 { out.push(' '); } out.push(c); }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BarForm { Full, Short, Chips }
#[derive(Clone, Debug, PartialEq)]
pub struct Placed { pub x: f64, pub width: f64, pub chip: Option<usize>, pub text: String, pub form: BarForm }

/// Pure responsive packing. `measure` supplies cached label widths in logical pixels; the widget
/// uses LabelCache, and tests can supply deterministic metrics. Narrow chips omit state words.
pub fn source_bar_layout(width: f64, sources: &[SourceChip], answers: &Answers, mut measure: impl FnMut(&str) -> f64) -> Vec<Placed> {
    for form in [BarForm::Full, BarForm::Short, BarForm::Chips] {
        let mut placed = Vec::new();
        let mut x = 0.0;
        for (i, source) in sources.iter().enumerate() {
            let text = if form != BarForm::Chips && source.state != SourceState::Live { format!("{} {}", source.info.name, source.state_text()) } else { source.info.name.clone() };
            let w = 20.0 + measure(&text);
            placed.push(Placed { x, width: w, chip: Some(i), text, form });
            x += w + 16.0;
        }
        if form != BarForm::Chips {
            for text in answers.text(form == BarForm::Short) {
                let w = measure(&text);
                placed.push(Placed { x, width: w, chip: None, text, form });
                x += w + 16.0;
            }
        }
        if x - 16.0 <= width || form == BarForm::Chips { return placed; }
    }
    Vec::new()
}

/// Sentences describing only missing capabilities. Exact, unbounded simulation is never
/// described as quantised or capture-limited; absent hardware alone is not a missing trigger.
pub fn cannot(caps: &Caps) -> Vec<String> {
    let mut out = Vec::new();
    if !caps.powers.current { out.push("It cannot measure current.".into()); }
    if !caps.powers.rewind {
        if let Depth::Samples(n) = caps.depth { out.push(format!("It keeps at most {} samples per capture, so it cannot rewind before a capture began.", grouped(n))); }
        else { out.push("It cannot rewind its input.".into()); }
    }
    if !caps.powers.members { out.push("It cannot run ensemble members.".into()); }
    if !caps.powers.probe_any_net { out.push("It cannot probe a net without an input connection.".into()); }
    if !caps.powers.zero_load { out.push("It cannot measure without loading the circuit.".into()); }
    if !caps.modes.stream && caps.modes.block {
        if caps.trigger.hardware == [TriggerKind::Edge] {
            out.push("It triggers on edges in hardware; other trigger kinds run in software and miss what happens between captures.".into());
        } else { out.push("It cannot observe what happens between captures.".into()); }
    }
    out
}

/// Blind share for block sources, including hardware-triggered captures without an Over value.
pub fn block_blind(caps: &Caps, resolved: &Resolved) -> Option<f64> {
    if resolved.mode == Mode::Stream { return None; }
    if let TrigResolved::Software { over: Over::Blocks { blind } } = resolved.trigger { return Some(blind as f64); }
    caps.modes.block_period.map(|period| (1.0 - resolved.depth as f64 / resolved.rate / period).clamp(0.0, 1.0))
}

/// Exact circuit-simulator inputs and powers, used by the bench's source sheet.
pub fn simulation_caps(channels: Vec<ChannelInfo>) -> Caps {
    Caps { generation: 0, channels: channels.into_iter().map(|c| ChannelCaps {
        id: c.id, name: c.name, quantity: c.quantity, kind: ChanKind::Analog, ranges: Ranges::Any, bits: 0,
        coupling: vec![Coupling::Dc], attenuation: Atten::Free, bandwidth: None, bw_limits: Vec::new(),
    }).collect(), rate: RateCaps::Any, depth: Depth::Unbounded, modes: Modes { stream: true, ..Modes::default() },
        stream_rate: None, trigger: TriggerCaps::default(), outputs: Vec::new(),
        powers: Powers { probe_any_net: true, current: true, rewind: true, zero_load: true, members: true, exact_marks: true, ..Powers::default() } }
}

/// The source sheet's complete text, prepared on metadata changes or on opening the popover.
pub fn source_sheet(source: &SourceChip, answers: &Answers) -> String {
    let mut lines = vec![format!("{}{}", source.info.name, if source.state == SourceState::Live { String::new() } else { format!(" — {}", source.state_text()) })];
    lines.extend(answers.text(false));
    if source.caps.channels.iter().all(|c| c.bits == 0) { lines.push("Exact values.".into()); }
    if source.caps.depth == Depth::Unbounded { lines.push("Unbounded record.".into()); }
    if let Some(resolved) = &source.resolved {
        if source.caps.channels.iter().any(|c| c.bits > 0) || source.caps.depth != Depth::Unbounded {
            lines.push(format!("{}, {} samples per capture.", si_label(resolved.rate, "S/s"), grouped(resolved.depth)));
        } else { lines.push("Adaptive sampling over the circuit record.".into()); }
        for channel in &resolved.channels {
            let name = source.caps.channels.iter().find(|c| c.id == channel.id).map_or("Input", |c| c.name.as_str());
            lines.push(format!("{name}: {} to {}, {}.", si_label(channel.range.0, "V"), si_label(channel.range.1, "V"),
                if channel.step == 0.0 { "exact".into() } else { format!("{} steps", si_label(channel.step, "V")) }));
        }
        lines.push(match &resolved.trigger {
            TrigResolved::Hardware => if let Some(blind) = block_blind(&source.caps, resolved) {
                format!("{:?} trigger: hw, blind {:.1} % between captures.", source.trigger_kind, blind * 100.0)
            } else { format!("{:?} trigger: hw.", source.trigger_kind) },
            TrigResolved::Software { over: Over::Stream } => format!("{:?} trigger: sw over the stream.", source.trigger_kind),
            TrigResolved::Software { over: Over::Blocks { blind } } => format!("{:?} trigger: sw, blind {:.1} % between captures.", source.trigger_kind, blind * 100.0),
            TrigResolved::Unavailable(reason) => format!("Trigger unavailable: {reason}."),
        });
        lines.extend(resolved.notes.iter().map(|note| note.text.clone()));
    } else { lines.push("Waiting for acquisition settings.".into()); }
    let missing = cannot(&source.caps);
    if !missing.is_empty() { lines.push(String::new()); lines.extend(missing); }
    lines.join("\n")
}

script_mod! {
    use mod.prelude.widgets_internal.*
    use mod.widgets.*

    let SourceSide = set_type_default() do #(SourceSide::script_api(vm))

    set_type_default() do #(DrawSourceSide::script_shader(vm)) {
        ..mod.draw.DrawQuad
        color: #fff
        pixel: fn() {
            let p = self.pos * self.rect_size
            let sdf = Sdf2d.viewport(p)
            sdf.circle(6., 6., 4.)
            match self.side {
                SourceSide.Real => { sdf.fill(self.color) }
                SourceSide.Replay => {
                    sdf.fill(self.color)
                    sdf.move_to(8., 8.)
                    sdf.line_to(8., 3.)
                    sdf.line_to(11., 3.)
                    sdf.stroke(self.color, 1.2)
                }
                _ => { sdf.stroke(self.color, 1.3) }
            }
            return sdf.result
        }
    }

    mod.widgets.SourceBarBase = #(SourceBar::register_widget(vm))
    mod.widgets.SourceBar = set_type_default() do mod.widgets.SourceBarBase {
        width: Fill height: 30
        source_sheet: Popover {
            trigger: Manual trap_focus: true
            content := View {
                width: 480 height: Fit flow: Down
                body := Label { width: Fill height: Fit draw_text +: {text_style: theme.font_regular {font_size: 11}} }
            }
        }
        draw_text +: {color: theme.color_text text_style: theme.font_code {font_size: 10}}
    }
}

#[derive(Script, ScriptHook)]
#[repr(C)]
struct DrawSourceSide {
    #[deref] draw_super: DrawQuad,
    #[live] color: Vec4f,
    #[live] side: SourceSide,
}

/// The source dot's shape: an outline for the simulator, solid for a real input, solid with a
/// clock tick for a replay.
#[derive(Clone, Copy, Debug, PartialEq, Script, ScriptHook)]
#[repr(u32)]
enum SourceSide { #[pick] Sim = 1, Real = 2, Replay = 3 }

#[derive(Clone, Debug, Default)]
pub enum SourceBarAction {
    SeeAs(SeeAs),
    Look { persistence: Persistence, intensity: f64 },
    Clear,
    #[default]
    None,
}

#[derive(Clone, Copy, PartialEq)]
enum Part { Afterglow, Grade }

#[derive(Script, ScriptHook, WidgetRegister, WidgetRef, WidgetSet)]
pub struct SourceBar {
    #[uid] uid: WidgetUid,
    #[source] source: ScriptObjectRef,
    #[walk] walk: Walk,
    #[live] draw_shape: DrawColor,
    #[live] draw_text: DrawText,
    #[rust] cache: LabelCache,
    #[rust] area: Area,
    #[rust] drawn_at: DVec2,
    #[rust] afterglow: Rect,
    #[rust] grade: Rect,
    #[rust] drag: Option<(Part, f64, f64)>,
    #[rust] persistence: Persistence,
    #[rust(1000.0)] intensity: f64,
    #[rust] channel: usize,
    #[rust] label: String,
    #[rust] ramp_key: Option<(bool, usize, f64)>,
    #[rust] ramp: Vec<[f32; 4]>,
    #[live] draw_side: DrawSourceSide,
    #[live] source_sheet: ScriptObjectRef,
    #[rust] sheet: Option<WidgetRef>,
    #[rust] open_source: Option<usize>,
    #[rust] sheet_dirty: bool,
    /// The sheet was drawn open on the last draw.
    #[rust] sheet_shown: bool,
    #[rust] sources: Vec<SourceChip>,
    #[rust] answers: Answers,
    #[rust] placed: Vec<Placed>,
    #[rust] layout_width: Option<f64>,
    #[rust] chip_rects: Vec<Rect>,
    #[rust] see_as: Option<SeeAs>,
    #[rust] see_as_rect: Rect,
}

impl SourceBar {
    pub fn set_see_as(&mut self, see_as: SeeAs) -> bool {
        if self.see_as == Some(see_as) { return false; }
        self.see_as = Some(see_as);
        self.layout_width = None;
        true
    }

    fn choose_see_as(&mut self, cx: &mut Cx, see_as: SeeAs) {
        if self.set_see_as(see_as) {
            self.area.redraw(cx);
            cx.widget_action(self.uid, SourceBarAction::SeeAs(see_as));
        }
    }

    /// Replace source snapshots only when membership, state or applied settings change. Returns
    /// whether the host should redraw. Unchanged chips do no formatting or measuring per frame.
    pub fn set_sources(&mut self, sources: &[SourceChip]) -> bool {
        if self.sources == sources { return false; }
        self.open_source = self.open_source.and_then(|i| self.sources.get(i)).and_then(|old| sources.iter().position(|s| s.info.id == old.info.id));
        self.sources.clear(); self.sources.extend_from_slice(sources);
        self.layout_width = None; self.sheet_dirty = true;
        true
    }

    pub fn set_answers(&mut self, cx: &mut Cx, mut answers: Answers) {
        if self.see_as == Some(SeeAs::BenchScope) {
            if let Some(blind) = self.sources.first().and_then(|s| s.resolved.as_ref().and_then(|r| block_blind(&s.caps, r))) {
                answers.coverage = Some(match answers.coverage {
                    Some(Coverage::All { triggers }) => Coverage::Blocks { captures: triggers, blind },
                    Some(Coverage::Dropped { blind: lost }) => Coverage::Dropped { blind: 1.0 - (1.0 - blind) * (1.0 - lost) },
                    Some(blocks @ Coverage::Blocks { .. }) => blocks,
                    None => Coverage::Blocks { captures: 0.0, blind },
                });
            }
        }
        if self.answers != answers {
            self.answers = answers; self.layout_width = None; self.sheet_dirty = true; self.area.redraw(cx);
        }
    }

    fn sheet_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        if self.sheet.is_none() {
            self.sheet = Some(cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.source_sheet.as_object().into())));
            cx.widget_tree_mark_dirty(self.uid);
        }
        self.sheet.as_ref().unwrap().clone()
    }

    fn update_sheet(&mut self, cx: &mut Cx) {
        if !self.sheet_dirty { return; }
        if let Some(source) = self.open_source.and_then(|i| self.sources.get(i)) {
            let text = source_sheet(source, &self.answers);
            let widget = self.sheet_widget(cx);
            widget.as_popover().content().label(cx, ids!(body)).set_text(cx, &text);
            self.sheet_dirty = false;
        }
    }

    fn open_sheet(&mut self, cx: &mut Cx, index: usize) {
        self.open_source = Some(index); self.sheet_dirty = true;
        self.update_sheet(cx);
        let widget = self.sheet_widget(cx);
        widget.as_popover().open_at(cx, self.chip_rects[index]);
        self.area.redraw(cx);
    }

    pub fn set_look(&mut self, cx: &mut Cx, persistence: Persistence, intensity: f64, channel: usize) {
        if self.label.is_empty() || self.persistence != persistence || self.intensity != intensity || self.channel != channel {
            self.persistence = persistence;
            self.intensity = intensity;
            self.channel = channel;
            self.label = match persistence {
                Persistence::Off => "off".into(), Persistence::Infinite => "forever".into(),
                Persistence::Seconds(t) => si_label(t, "s"),
            };
            self.area.redraw(cx);
        }
    }

    fn fraction(&self, part: Part) -> f64 {
        match part { Part::Afterglow => afterglow_position(self.persistence), Part::Grade => (self.intensity / INTENSITY_MIN).log10() / (INTENSITY_MAX / INTENSITY_MIN).log10() }
    }

    fn change(&mut self, cx: &mut Cx, part: Part, position: f64) {
        let position = position.clamp(0.0, 1.0);
        let (persistence, intensity) = match part {
            Part::Afterglow => (afterglow_at(position), self.intensity),
            Part::Grade => (self.persistence, INTENSITY_MIN * (INTENSITY_MAX / INTENSITY_MIN).powf(position)),
        };
        if persistence == self.persistence && intensity == self.intensity { return; }
        self.set_look(cx, persistence, intensity, self.channel);
        cx.widget_action(self.uid, SourceBarAction::Look { persistence, intensity });
    }

    fn quad(&mut self, cx: &mut Cx2d, rect: Rect, color: [f32; 4]) {
        self.draw_shape.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_shape.draw_abs(cx, rect);
    }

    fn text(&mut self, cx: &mut Cx2d, pos: DVec2, text: &str, color: [f32; 4]) { self.text_at(cx, pos, 0.0, text, color); }

    /// `anchor` 0 puts the text's left edge at `pos.x`, 1 its right edge.
    fn text_at(&mut self, cx: &mut Cx2d, pos: DVec2, anchor: f64, text: &str, color: [f32; 4]) {
        let layout = self.cache.layout(cx, &self.draw_text, text);
        let size = dvec2(f64::from(layout.size_in_lpxs.width * self.draw_text.font_scale), f64::from(layout.size_in_lpxs.height * self.draw_text.font_scale));
        let pos = dvec2(pos.x - size.x * anchor, pos.y);
        self.draw_text.color = vec4(color[0], color[1], color[2], color[3]);
        self.draw_text.draw_walk_laidout(cx, Walk::abs_rect(Rect { pos, size }), &layout);
    }
}

impl WidgetNode for SourceBar {
    fn widget_uid(&self) -> WidgetUid { self.uid }
    fn walk(&mut self, _: &mut Cx) -> Walk { self.walk }
    fn area(&self) -> Area { self.area }
    fn redraw(&mut self, cx: &mut Cx) { self.area.redraw(cx); }
    fn children(&self, visit: &mut dyn FnMut(LiveId, WidgetRef)) { if let Some(sheet) = &self.sheet { visit(id!(source_sheet), sheet.clone()); } }
    fn find_widgets_from_point(&self, cx: &Cx, point: DVec2, found: &mut dyn FnMut(&WidgetRef)) { if let Some(sheet) = &self.sheet { sheet.find_widgets_from_point(cx, point, found); } }
}

impl Widget for SourceBar {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        let theme = base_theme(cx);
        let dark = theme != BaseTheme::Light;
        let look = InstrumentLook::for_theme(dark);
        if self.cache.begin_frame(&self.draw_text, theme) { self.layout_width = None; }
        let view = cx.walk_turtle(walk);
        cx.add_rect_area(&mut self.area, view);
        if self.see_as.is_some() { cx.add_nav_stop(self.area, NavRole::TextInput, Inset::default()); }
        self.drawn_at = view.pos;
        let controls = view.size.x >= if self.see_as.is_some() { 800.0 } else { 600.0 };
        let right = view.pos.x + view.size.x - 12.0;
        self.grade = Rect { pos: dvec2(right - 144.0, view.pos.y), size: dvec2(144.0, view.size.y) };
        self.afterglow = Rect { pos: dvec2(right - 282.0, view.pos.y), size: dvec2(120.0, view.size.y) };
        cx.push_clip_rect(view);
        if self.ramp_key != Some((dark, self.channel, self.intensity)) {
            let channel = look.channel(self.channel);
            let screen = look.canvas.canvas;
            self.ramp.clear();
            for i in 0..32 {
                // Log-spaced occupancy, from one in ten thousand sweeps to every sweep.
                let p = 10.0_f32.powf(-4.0 + 4.0 * i as f32 / 31.0);
                let x = grade_x(p * self.intensity as f32, self.intensity as f32);
                let a = (x * 8.0).clamp(0.0, 1.0);
                let mut color = [0.0, 0.0, 0.0, 1.0];
                for k in 0..3 {
                    // The same channel ramp and coverage as PhosphorView's grade shader.
                    let grade = if dark { channel[k] * 0.15 + 0.85 * x }
                        else { screen[k] + (channel[k] - screen[k]) * (0.15 + 0.85 * x) };
                    color[k] = screen[k] + (grade - screen[k]) * a;
                }
                self.ramp.push(color);
            }
            self.ramp_key = Some((dark, self.channel, self.intensity));
        }
        if controls {
            let p = self.afterglow.pos;
            for i in 0..9 {
                let ink = look.channel(self.channel);
                self.quad(cx, Rect { pos: p + dvec2(i as f64 * 3.0, 13.0), size: dvec2(3.0, 3.0) }, [ink[0], ink[1], ink[2], (i + 1) as f32 / 9.0]);
            }
            let label = std::mem::take(&mut self.label);
            self.text(cx, p + dvec2(34.0, 8.0), &label, look.text_secondary);
            self.label = label;
            let p = self.grade.pos;
            for i in 0..32 {
                self.quad(cx, Rect { pos: p + dvec2(i as f64 * 4.5, 16.0), size: dvec2(4.5, 7.0) }, self.ramp[i]);
            }
            self.text(cx, p + dvec2(0.0, 1.0), "rare", look.text_tertiary);
            self.text_at(cx, p + dvec2(self.grade.size.x, 1.0), 1.0, "always", look.text_tertiary);
            let marker = p + dvec2(self.fraction(Part::Grade) * 142.0, 14.0);
            self.quad(cx, Rect { pos: marker, size: dvec2(2.0, 11.0) }, look.text);
        }
        cx.pop_clip_rect();
        // Controls yield at narrow widths so the source names remain reachable.
        let room = (view.size.x - if controls { 310.0 } else { 24.0 }).max(0.0);
        if !controls {
            self.afterglow = Rect::default(); self.grade = Rect::default();
        }
        if self.layout_width != Some(room) {
            let cache = &mut self.cache;
            let draw = &self.draw_text;
            let see_as_space = if self.see_as.is_some() { 176.0 } else { 0.0 };
            self.placed = source_bar_layout((room - see_as_space).max(0.0), &self.sources, &self.answers, |text| {
                f64::from(cache.layout(cx, draw, text).size_in_lpxs.width * draw.font_scale)
            });
            for item in self.placed.iter_mut().skip(1) { item.x += see_as_space; }
            self.layout_width = Some(room);
        }
        self.chip_rects.resize(self.sources.len(), Rect::default());
        cx.push_clip_rect(Rect { pos: view.pos, size: dvec2(room + 12.0, view.size.y) });
        let placed = std::mem::take(&mut self.placed);
        for item in &placed {
            let mut pos = view.pos + dvec2(12.0 + item.x, 8.0);
            if let Some(i) = item.chip {
                self.chip_rects[i] = Rect { pos: view.pos + dvec2(12.0 + item.x, 0.0), size: dvec2(item.width, view.size.y) };
                self.draw_side.side = match self.sources[i].info.side { Side::Sim => SourceSide::Sim, Side::Real => SourceSide::Real, Side::Replay => SourceSide::Replay };
                let ink = look.text;
                self.draw_side.color = vec4(ink[0], ink[1], ink[2], ink[3]);
                self.draw_side.draw_abs(cx, Rect { pos, size: dvec2(12.0, 12.0) });
                pos.x += 20.0;
            }
            self.text(cx, pos, &item.text, look.text_secondary);
        }
        self.placed = placed;
        self.see_as_rect = Rect::default();
        if let Some(see_as) = self.see_as {
            let x = self.chip_rects.first().map_or(view.pos.x + 12.0, |r| r.pos.x + r.size.x + 16.0);
            self.see_as_rect = Rect { pos: dvec2(x, view.pos.y + 3.0), size: dvec2(160.0, 24.0) };
            self.quad(cx, self.see_as_rect, look.canvas.canvas);
            for (choice, offset, width) in [(SeeAs::Exact, 0.0, 54.0), (SeeAs::BenchScope, 54.0, 106.0)] {
                let rect = Rect { pos: self.see_as_rect.pos + dvec2(offset, 0.0), size: dvec2(width, 24.0) };
                if choice == see_as { self.quad(cx, rect, look.text_secondary); }
                self.text(cx, rect.pos + dvec2(9.0, 5.0), choice.label(), if choice == see_as { look.canvas.canvas } else { look.text_secondary });
            }
            if cx.has_key_focus(self.area) {
                self.quad(cx, Rect { pos: self.see_as_rect.pos + dvec2(0.0, 23.0), size: dvec2(160.0, 1.0) }, look.text);
            }
        }
        cx.pop_clip_rect();
        // The sheet is made at its first open and drawn while open, plus once after it closes: an
        // overlay list that is not begun keeps showing its last picture.
        if let Some(sheet) = self.sheet.clone() {
            if self.open_source.is_none_or(|i| i >= self.sources.len()) { sheet.as_popover().close(cx); }
            let open = sheet.as_popover().is_open();
            if open { self.update_sheet(cx); }
            if open || self.sheet_shown {
                sheet.draw_walk_all(cx, scope, Walk::abs_rect(self.open_source.and_then(|i| self.chip_rects.get(i).copied()).unwrap_or_default()));
            }
            self.sheet_shown = open;
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        let shift = self.area.rect(cx).pos - self.drawn_at;
        self.afterglow.pos += shift;
        self.grade.pos += shift;
        self.see_as_rect.pos += shift;
        self.drawn_at += shift;
        for rect in &mut self.chip_rects { rect.pos += shift; }
        if let Some(sheet) = &self.sheet {
            let popover = sheet.as_popover();
            let was_open = popover.is_open();
            if was_open && shift != dvec2(0.0, 0.0) {
                if let Some(rect) = self.open_source.and_then(|i| self.chip_rects.get(i)) { popover.open_at(cx, *rect); }
            }
            sheet.handle_event(cx, event, scope);
            if was_open { return; }
        }
        match event.hits(cx, self.area) {
            Hit::FingerHoverIn(e) | Hit::FingerHoverOver(e) => {
                cx.set_cursor(if self.afterglow.contains(e.abs) || self.grade.contains(e.abs) || self.see_as.is_some() && self.see_as_rect.contains(e.abs) || self.chip_rects.iter().any(|r| r.contains(e.abs)) { MouseCursor::Hand } else { MouseCursor::Default });
            }
            Hit::FingerHoverOut(_) if self.drag.is_none() => cx.set_cursor(MouseCursor::Default),
            Hit::FingerDown(e) if e.device.is_primary_hit() => {
                if self.see_as.is_some() && self.see_as_rect.contains(e.abs) {
                    cx.set_key_focus_from_user(self.area);
                    self.choose_see_as(cx, if e.abs.x < self.see_as_rect.pos.x + 54.0 { SeeAs::Exact } else { SeeAs::BenchScope });
                    return;
                }
                if let Some(i) = self.chip_rects.iter().position(|r| r.contains(e.abs)) { self.open_sheet(cx, i); return; }
                let part = if self.afterglow.contains(e.abs) { Part::Afterglow } else if self.grade.contains(e.abs) { Part::Grade } else { return; };
                if part == Part::Afterglow && e.tap_count == 2 { self.drag = None; cx.widget_action(self.uid, SourceBarAction::Clear); }
                else { self.drag = Some((part, e.abs.x, self.fraction(part))); }
            }
            Hit::FingerMove(e) => {
                if let Some((part, x, start)) = self.drag {
                    let width = if part == Part::Afterglow { self.afterglow.size.x } else { self.grade.size.x };
                    self.change(cx, part, start + (e.abs.x - x) / width.max(1.0));
                }
            }
            Hit::FingerUp(_) => self.drag = None,
            Hit::KeyFocus(_) | Hit::KeyFocusLost(_) => self.area.redraw(cx),
            Hit::KeyDown(e) if !e.is_repeat && matches!(e.key_code, KeyCode::Space | KeyCode::ReturnKey | KeyCode::NumpadEnter) => {
                if let Some(see_as) = self.see_as { self.choose_see_as(cx, see_as.other()); }
            }
            _ => {}
        }
    }

    fn semantics_parts(&self, cx: &Cx, out: &mut Vec<(u64, Option<u64>, a11y_tree::Node)>) {
        if let Some(see_as) = self.see_as {
            let mut node = a11y_tree::Node { role: a11y_tree::Role::Button, name: "see as".into(), value: see_as.label().into(),
                bounds: makepad_platform::a11y::semantic_rect(Rect { pos: self.see_as_rect.pos + self.area.rect(cx).pos - self.drawn_at, ..self.see_as_rect }),
                actions: a11y_tree::Actions::PRESS | a11y_tree::Actions::FOCUS, states: a11y_tree::States::FOCUSABLE, ..Default::default() };
            node.states.set(a11y_tree::States::FOCUSED, cx.has_key_focus(self.area));
            out.push((3, None, node));
        }
        for (i, source) in self.sources.iter().enumerate() {
            if let Some(rect) = self.chip_rects.get(i) {
                out.push((100 + i as u64, None, a11y_tree::Node { role: a11y_tree::Role::Button,
                    name: format!("{} source details", source.info.name), bounds: makepad_platform::a11y::semantic_rect(Rect { pos: rect.pos + self.area.rect(cx).pos - self.drawn_at, ..*rect }),
                    actions: a11y_tree::Actions::PRESS, ..Default::default() }));
            }
        }
        for (key, name, rect, position) in [(1, "afterglow", self.afterglow, self.fraction(Part::Afterglow)), (2, "intensity: rare to always", self.grade, self.fraction(Part::Grade))] {
            if rect.size.x <= 0.0 { continue; }
            out.push((key, None, a11y_tree::Node {
                role: a11y_tree::Role::Slider, name: name.into(),
                bounds: makepad_platform::a11y::semantic_rect(Rect { pos: rect.pos + self.area.rect(cx).pos - self.drawn_at, ..rect }),
                range: Some(a11y_tree::RangeValue { min: 0.0, max: 1.0, now: position, step: 0.02 }),
                actions: a11y_tree::Actions::INCREMENT | a11y_tree::Actions::DECREMENT,
                ..Default::default()
            }));
        }
    }

    fn handle_action(&mut self, cx: &mut Cx, request: &a11y_tree::ActionRequest, _: &mut Scope) -> a11y_tree::ActionResult {
        use a11y_tree::{ActionResult, Actions};
        if cx.a11y.action_part_key(request.node) == Some(3) {
            let Some(see_as) = self.see_as else { return ActionResult::Unhandled };
            if request.action == Actions::FOCUS { cx.set_key_focus(self.area); self.area.redraw(cx); }
            else if request.action == Actions::PRESS { self.choose_see_as(cx, see_as.other()); }
            else { return ActionResult::Unhandled; }
            return ActionResult::Handled;
        }
        if let Some(key) = cx.a11y.action_part_key(request.node).filter(|key| *key >= 100) {
            let i = (key - 100) as usize;
            if request.action == Actions::PRESS && i < self.chip_rects.len() { self.open_sheet(cx, i); return ActionResult::Handled; }
        }
        let part = match cx.a11y.action_part_key(request.node) { Some(1) => Part::Afterglow, Some(2) => Part::Grade, _ => return ActionResult::Unhandled };
        let delta = if request.action == Actions::INCREMENT { 0.02 } else if request.action == Actions::DECREMENT { -0.02 } else { return ActionResult::Unhandled };
        self.change(cx, part, self.fraction(part) + delta);
        ActionResult::Handled
    }
}

//! Lane-state lookup, shared typed edits and the value-entry overlay.

use super::*;
use crate::instruments::semantics::{self, Lookup};
use a11y_tree::{ActionArg, ActionResult, Actions as A, Node, Role, States};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Item { Control(Part), TimeBracket, TimeEnd(End), ValueBracket, ValueEnd(End), Holdoff }

impl Item {
    fn key(self) -> u64 {
        match self {
            Self::Control(part) => part.key(), Self::Holdoff => 14,
            Self::TimeBracket => 400, Self::TimeEnd(End::Start) => 401, Self::TimeEnd(End::Finish) => 402,
            Self::ValueBracket => 403, Self::ValueEnd(End::Start) => 404, Self::ValueEnd(End::Finish) => 405,
        }
    }

    fn numeric(self) -> bool {
        !matches!(self, Self::Control(part) if !matches!(part, Part::Resize | Part::Time | Part::Scale(_) | Part::Offset(_) | Part::LevelMarker | Part::RuntLevel | Part::PositionMarker | Part::Divider | Part::CellDivider | Part::SpectrumScale | Part::SpectrumOffset | Part::Frequency))
    }
}

// Wrapped answers already have widths and row ranges cached on layout changes. Derive their
// hit geometry on lookup/pointer events, without adding per-frame parts or semantic nodes.
fn wrapped_rect(widths: &[f64], indices: &[usize], rows: &[Range<usize>], origin: DVec2, index: usize) -> Option<Rect> {
    let index = indices.binary_search(&index).ok()?;
    let (row, range) = rows.iter().enumerate().find(|(_, range)| range.contains(&index))?;
    let x = widths.get(range.start..index)?.iter().map(|width| width + ANSWER_GAP).sum::<f64>();
    Some(Rect { pos: origin + dvec2(x, row as f64 * ROW), size: dvec2(*widths.get(index)?, 22.0) })
}

struct Items<'a> {
    net: &'a str,
    settings: &'a ScopeSettings,
    answers: &'a Answers,
    names: &'a [Option<&'static str>],
    parts: &'a [PartRect],
    widths: &'a [f64],
    indices: &'a [usize],
    rows: &'a [Range<usize>],
    rows_origin: DVec2,
    sources: &'a [String],
    primary: usize,
    more: usize,
    trigger: bool,
    shared: bool,
    collapsed: bool,
    closable: bool,
    bracket_visible: bool,
    bracket: Option<(f64, f64)>,
    values: Option<(f64, f64)>,
    delays: &'a [DelayMark],
    plot: Rect,
    shift: DVec2,
    clip: Option<Rect>,
    time: Option<Rect>,
    holdoff: Option<Rect>,
    focused: Option<Part>,
    lane: Option<&'a SignalLane>,
}

impl Items<'_> {
    fn label(&self) -> &str { self.settings.channels.first().map_or(self.net, Channel::title) }
    fn unit(&self) -> &str { self.settings.channels.first().map_or("V", |c| c.unit.as_str()) }
    fn key(&self, item: Item) -> u64 { semantics::key("scope lane", self.net, item.key()) }

    fn each(&self, mut visit: impl FnMut(Item)) {
        use Item::*;
        for part in [Part::Tag, Part::TriggerChannel, Part::Collapse, Part::Domain, Part::AddAnswer, Part::Resize] { visit(Control(part)); }
        if self.closable { visit(Control(Part::Close)); }
        if self.sources.len() > 1 { visit(Control(Part::Secondary)); }
        if self.more > 0 { visit(Control(Part::More)); }
        for i in 0..self.answers.items.len() { visit(Control(Part::Answer(i))); }
        for i in 0..self.settings.channels.len() {
            visit(Control(Part::Scale(i))); visit(Control(Part::Offset(i)));
        }
        if self.trigger || !self.shared {
            visit(Control(Part::Time)); visit(Control(Part::LevelMarker)); visit(Control(Part::Slope));
            if self.settings.trigger.qualify != Qualifier::None { visit(Control(Part::Qualifier)); }
            if matches!(self.settings.trigger.qualify, Qualifier::Runt { .. }) { visit(Control(Part::RuntLevel)); }
            visit(Control(Part::PositionMarker)); visit(Holdoff);
        }
        if self.settings.domain == Domain::Fft && !self.collapsed {
            for part in [Part::Spectrum, Part::SpectrumScale, Part::SpectrumOffset, Part::Frequency, Part::CellDivider] { visit(Control(part)); }
            if self.settings.fft.waterfall { visit(Control(Part::Divider)); }
        }
        if self.lane.is_some_and(|lane| lane.lane_zone().is_some()) { visit(Control(Part::Zone)); }
        if self.bracket.is_some() {
            visit(TimeBracket); visit(TimeEnd(End::Start)); visit(TimeEnd(End::Finish));
        }
        if self.values.is_some() {
            visit(ValueBracket); visit(ValueEnd(End::Start)); visit(ValueEnd(End::Finish));
        }
    }

    fn find(&self, key: u64) -> Option<Item> {
        let mut found = None;
        self.each(|item| { if self.key(item) == key { found = Some(item); } });
        found
    }

    fn x(&self, t: f64) -> f64 {
        let window = self.settings.window();
        self.plot.pos.x + (t + window.pre) / (window.pre + window.post) * self.plot.size.x
    }

    fn y(&self, value: f64) -> f64 {
        let map = self.settings.map((1, 1));
        self.plot.pos.y + (map.v_top - value) / map.v_px * self.plot.size.y
    }

    // Same precedence as FingerDown: resize edge, runt marker, zone, delay ends, value ends, time ends, then other parts.
    fn hit(&self, point: DVec2) -> Option<Item> {
        if self.holdoff.is_some_and(|rect| rect.contains(point)) { return Some(Item::Holdoff); }
        if self.shared && self.trigger && self.time.is_some_and(|rect| rect.contains(point)) {
            return self.parts.iter().rev().find(|p| p.part == Part::Time && p.rect.contains(point)).map(|_| Item::Control(Part::Time));
        }
        let point = point - self.shift;
        if self.parts.iter().any(|part| part.part == Part::Resize && part.rect.contains(point)) { return Some(Item::Control(Part::Resize)); }
        if self.parts.iter().rev().find(|part| part.rect.contains(point)).is_some_and(|part| part.part == Part::RuntLevel) { return Some(Item::Control(Part::RuntLevel)); }
        if self.parts.iter().any(|part| part.part == Part::CellDivider && part.rect.contains(point)) { return Some(Item::Control(Part::CellDivider)); }
        if !self.collapsed && self.plot.contains(point) {
            if self.parts.iter().any(|part| part.part == Part::Zone && part.rect.contains(point)) { return Some(Item::Control(Part::Zone)); }
            if self.bracket_visible && self.delays.iter().any(|mark| mark.end.is_some() && mark.feature.is_some_and(|edge| (point.x - self.x(edge.t)).abs() <= 4.0)) { return None; }
            if let Some((a, b)) = self.values {
                if let Some(end) = value_bracket_end_at(point.y, self.y(a), self.y(b)) { return Some(Item::ValueEnd(end)); }
            }
            if self.bracket_visible {
                if let Some((a, b)) = self.bracket {
                    if let Some(end) = bracket_end_at(point.x, self.x(a), self.x(b), 4.0) { return Some(Item::TimeEnd(end)); }
                }
            }
        }
        self.parts.iter().rev().find(|part| !(self.shared && self.trigger && part.part == Part::Time) && part.rect.contains(point)).map(|part| Item::Control(part.part))
            .or_else(|| self.rows.iter().flat_map(|range| range.clone()).filter_map(|i| self.indices.get(i).copied()).find(|&i| wrapped_rect(self.widths, self.indices, self.rows, self.rows_origin, i).is_some_and(|rect| rect.contains(point))).map(|i| Item::Control(Part::Answer(i))))
    }

    fn rect(&self, item: Item) -> Option<Rect> {
        if item == Item::Holdoff { return self.holdoff; }
        let shared_time = item == Item::Control(Part::Time) && self.shared && self.trigger;
        let clip = if shared_time { self.time? } else { self.clip? };
        let rect = match item {
            Item::Control(part) => self.parts.iter().rev().find(|p| p.part == part).map(|p| p.rect)
                .or_else(|| match part { Part::Answer(i) => wrapped_rect(self.widths, self.indices, self.rows, self.rows_origin, i), _ => None })?,
            Item::TimeBracket | Item::TimeEnd(_) | Item::ValueBracket | Item::ValueEnd(_) => {
                if self.collapsed { return None; }
                let p = self.plot;
                match item {
                    Item::TimeBracket | Item::TimeEnd(_) => {
                        if !self.bracket_visible { return None; }
                        let (a, b) = self.bracket?;
                        let t = if item == Item::TimeEnd(End::Finish) { b } else { a };
                        Rect { pos: dvec2(self.x(t) - 2.0, p.pos.y), size: dvec2(4.0, p.size.y) }
                    }
                    _ => {
                        let (a, b) = self.values?;
                        let v = if item == Item::ValueEnd(End::Finish) { b } else { a };
                        Rect { pos: dvec2(p.pos.x, self.y(v) - 2.0), size: dvec2(p.size.x, 4.0) }
                    }
                }
            }
            Item::Holdoff => return None,
        };
        let rect = semantics::intersect(Rect { pos: rect.pos + if shared_time { dvec2(0.0, 0.0) } else { self.shift }, ..rect }, clip)?;
        // Check the serialized centre, just as the board's picker does. Small press boxes avoid
        // crossing a bracket or another handle when clipping changes the original centre.
        for y in [0.5, 0.25, 0.75, 0.01, 0.99] {
            for x in [0.5, 0.25, 0.75, 0.01, 0.99] {
                let point = rect.pos + dvec2(rect.size.x * x, rect.size.y * y);
                let size = dvec2(2.0f64.min(rect.size.x), 2.0f64.min(rect.size.y));
                let candidate = semantics::intersect(Rect { pos: point - size * 0.5, size }, rect)?;
                let (x, y) = makepad_platform::a11y::semantic_rect(candidate).center();
                let hit = self.hit(dvec2(x as f64, y as f64));
                if hit == Some(item) || matches!((item, hit), (Item::TimeBracket, Some(Item::TimeEnd(_))) | (Item::ValueBracket, Some(Item::ValueEnd(_)))) { return Some(candidate); }
            }
        }
        None
    }

    fn node(&self, item: Item) -> Node {
        use Item::*;
        let mut node = Node { role: if item.numeric() { Role::Slider } else { Role::Button }, actions: A::FOCUS | A::PRESS, states: States::FOCUSABLE, ..Default::default() };
        if item.numeric() { node.actions.insert(A::SET_VALUE); }
        let end_name = |end| if end == End::Start { "start" } else { "end" };
        match item {
            Control(Part::Tag) => { node.role = Role::Group; node.name = self.label().into(); node.value = self.answers.text.clone(); }
            Control(Part::Resize) => { node.name = "height".into(); node.value = format!("{} pt", self.lane.map_or(DEFAULT_LANE_HEIGHT, |lane| lane.height)); }
            Control(Part::LevelMarker) => { node.name = "trigger".into(); node.value = semantics::trigger_value(self.label(), self.settings.trigger, self.unit()); }
            Control(Part::RuntLevel) => {
                node.name = "runt level".into();
                if let Qualifier::Runt { other } = self.settings.trigger.qualify { node.value = si_label(other, "V"); }
            }
            Control(Part::Zone) => {
                node.name = "trigger zone".into();
                if let Some(zone) = self.settings.trigger.zone {
                    node.value = format!("{} {} to {}, {} to {}", if zone.must { "must cross" } else { "must not cross" },
                        si_label(zone.t0, "s"), si_label(zone.t1, "s"), si_label(zone.v0, self.unit()), si_label(zone.v1, self.unit()));
                }
            }
            Control(Part::Qualifier) => { node.name = "trigger qualifier".into(); node.value = self.settings.trigger_qualifier_label(); }
            Control(Part::Time) => { node.name = "time".into(); node.value = format!("{}/div", si_label(self.settings.time_per_div, "s")); }
            Control(Part::Scale(i)) => {
                node.name = format!("{} scale", self.label());
                node.value = format!("{}/div", si_label(self.settings.channels[i].volts_per_div, &self.settings.channels[i].unit));
            }
            Control(Part::Offset(i)) => {
                node.name = format!("{} offset", self.label());
                node.value = si_label(self.settings.channels[i].offset_div * self.settings.channels[i].volts_per_div, &self.settings.channels[i].unit);
            }
            Control(Part::Answer(i)) => {
                let a = &self.answers.items[i];
                if self.names.get(i).copied().flatten().is_some_and(|name| self.lane.is_none_or(|lane| !lane.answer_expected(name))) { node.actions.insert(A::DISMISS); }
                node.name = a.name.trim().into();
                node.description = self.names.get(i).and_then(|name| *name).unwrap_or("answer").into();
                if node.name.is_empty() { node.name.clone_from(&node.description); }
                node.value = a.value.clone();
                if !a.unit.is_empty() { node.value.push(' '); node.value.push_str(&a.unit); }
                for (_, mark) in &a.marks { node.value.push(' '); node.value.push_str(mark); }
                if let Some(summary) = self.lane.and_then(|lane| lane.distribution_summary(i)) { node.value.push_str("; "); node.value.push_str(&summary); }
            }
            Control(Part::PositionMarker) => { node.name = "trigger position".into(); node.value = format!("{} %", self.settings.position * 100.0); }
            Control(Part::SpectrumScale) => { node.name = "spectrum axis".into(); node.value = format!("{} dB/div", self.settings.fft.db_per_div); }
            Control(Part::SpectrumOffset) => { node.name = "spectrum reference".into(); node.value = si_label(self.settings.fft.ref_dbv, "dBV"); }
            Control(Part::Frequency) => { node.name = "spectrum frequency span".into(); node.value = format!("{} record", si_label(10.0 * self.settings.time_per_div, "s")); }
            Control(Part::CellDivider) => { node.name = "spectrum cell divider".into(); node.value = format!("{} %", self.settings.fft.cell_share * 100.0); }
            Control(Part::Divider) => { node.name = "waterfall split".into(); node.value = format!("{} %", self.settings.fft.waterfall_share * 100.0); }
            Control(part) => { node.name = match part {
                Part::Secondary => format!("Show {} as primary", self.sources.get(1 - self.primary).map_or_else(|| "secondary source".into(), |name| name.replace('\n', " "))),
                Part::More => format!("{} more answers", self.more),
                Part::AddAnswer => "add measurement".into(),
                Part::TriggerChannel => "use as trigger".into(), Part::Collapse => "collapse".into(), Part::Domain => "spectrum".into(),
                Part::Close => "close".into(), Part::Slope => "trigger slope".into(), Part::Spectrum => format!("spectrum window {} settings", self.settings.fft.window_label()),
                _ => "pan".into(),
            }; }
            Holdoff => { node.name = "trigger holdoff".into(); node.value = si_label(self.settings.trigger.holdoff, "s"); }
            TimeBracket => { let (a, b) = self.bracket.unwrap(); node.name = "time bracket".into(); node.value = format!("{} .. {}", si_label(a, "s"), si_label(b, "s")); }
            TimeEnd(end) => { let (a, b) = self.bracket.unwrap(); node.name = format!("time bracket {}", end_name(end)); node.value = si_label(if end == End::Start { a } else { b }, "s"); }
            ValueBracket => { let (a, b) = self.values.unwrap(); node.name = format!("{} value bracket", self.label()); node.value = format!("{} .. {}", si_label(a, self.unit()), si_label(b, self.unit())); }
            ValueEnd(end) => { let (a, b) = self.values.unwrap(); node.name = format!("{} value bracket {}", self.label(), end_name(end)); node.value = si_label(if end == End::Start { a } else { b }, self.unit()); }
        }
        if let Control(part) = item {
            node.states.set(States::SELECTED, self.parts.iter().any(|p| p.part == part && p.selected));
            node.states.set(States::FOCUSED, self.focused == Some(part));
        }
        if item == Holdoff { node.states.set(States::FOCUSED, self.focused == Some(Part::Holdoff)); }
        semantics::bounds(&mut node, self.rect(item));
        node
    }

    fn lookup(&self, lookup: &Lookup, out: &mut Vec<(u64, Option<u64>, Node)>) {
        self.each(|item| { let key = self.key(item); if lookup.wants(key) { lookup.push(key, self.node(item), out); } });
    }
}

fn value_item(part: Part) -> Item {
    if part == Part::Holdoff { Item::Holdoff } else { Item::Control(part) }
}

fn value_unit(settings: &ScopeSettings, item: Item) -> Option<String> {
    Some(match item {
        Item::Control(Part::Time | Part::Frequency) => "s/div".to_owned(),
        Item::Control(Part::SpectrumScale) => "dB/div".into(),
        Item::Control(Part::SpectrumOffset) => "dBV".into(),
        Item::Control(Part::Scale(i)) => format!("{}/div", settings.channels.get(i)?.unit),
        Item::Control(Part::Offset(i)) => settings.channels.get(i)?.unit.clone(),
        Item::Control(Part::LevelMarker) => settings.channels.first().map_or("V", |c| c.unit.as_str()).into(),
        Item::Control(Part::RuntLevel) if matches!(settings.trigger.qualify, Qualifier::Runt { .. }) => "V".into(),
        Item::Control(Part::PositionMarker | Part::Divider | Part::CellDivider) => "%".into(),
        Item::Holdoff => "s".into(),
        _ => return None,
    })
}

fn value_parse_error(item: Item, unit: &str) -> String {
    match item {
        Item::Control(Part::Time | Part::Frequency) => "not a time per division".into(),
        Item::Control(Part::SpectrumScale | Part::Scale(_)) => format!("not a {} per division", unit.trim_end_matches("/div")),
        Item::Control(Part::PositionMarker | Part::Divider | Part::CellDivider) => "not a percentage".into(),
        Item::Holdoff => "not a time".into(),
        _ => format!("not a {unit} value"),
    }
}

fn value_range_error(item: Item, unit: &str) -> String {
    match item {
        Item::Control(Part::Time | Part::Frequency | Part::Scale(_)) => format!("out of range: {} to {}", si_label(DIV_MIN, unit), si_label(DIV_MAX, unit)),
        Item::Control(Part::SpectrumScale) => "out of range: 0.1 dB/div to 50 dB/div".into(),
        Item::Control(Part::PositionMarker) => "out of range: 0 % to 100 %".into(),
        Item::Control(Part::Divider) => "out of range: 15 % to 75 %".into(),
        Item::Control(Part::CellDivider) => "out of range: 20 % to 60 %".into(),
        Item::Control(Part::Offset(_)) => "out of range: a finite offset in divisions".into(),
        Item::Control(Part::RuntLevel) => "out of range: beyond the trigger hysteresis band on the pulse side".into(),
        Item::Holdoff => "out of range: 0 s or more, finite".into(),
        _ => format!("out of range: a finite {unit} value"),
    }
}

fn value_text(settings: &ScopeSettings, item: Item) -> Option<String> {
    let unit = value_unit(settings, item)?;
    let value = match item {
        Item::Control(Part::Time | Part::Frequency) => settings.time_per_div,
        Item::Control(Part::SpectrumScale) => return Some(format!("{} dB/div", settings.fft.db_per_div)),
        Item::Control(Part::SpectrumOffset) => settings.fft.ref_dbv,
        Item::Control(Part::Scale(i)) => settings.channels[i].volts_per_div,
        Item::Control(Part::Offset(i)) => settings.channels[i].offset_div * settings.channels[i].volts_per_div,
        Item::Control(Part::LevelMarker) => settings.trigger.level,
        Item::Control(Part::RuntLevel) => match settings.trigger.qualify { Qualifier::Runt { other } => other, _ => return None },
        Item::Control(Part::PositionMarker) => settings.position * 100.0,
        Item::Control(Part::Divider) => settings.fft.waterfall_share * 100.0,
        Item::Control(Part::CellDivider) => settings.fft.cell_share * 100.0,
        Item::Holdoff => settings.trigger.holdoff,
        _ => return None,
    };
    Some(si_label(value, &unit))
}

// The entry and SET_VALUE text action share parsing, range errors and continuous setters.
fn set_value_text(settings: &mut ScopeSettings, item: Item, text: &str) -> Result<Vec<ScopeAction>, String> {
    let unit = value_unit(settings, item).ok_or_else(|| "not a settable value".to_owned())?;
    let value = semantics::parse_number(text, &unit).ok_or_else(|| value_parse_error(item, &unit))?;
    apply_value(settings, item, value, &unit)
}

// Validate before touching settings. Keyboard stepping still uses its bounded 1-2-5 menu.
fn apply_value(settings: &mut ScopeSettings, item: Item, value: f64, unit: &str) -> Result<Vec<ScopeAction>, String> {
    if !value.is_finite() { return Err(value_range_error(item, unit)); }
    let actions = match item {
        Item::Control(Part::LevelMarker) => {
            if matches!(settings.trigger.qualify, Qualifier::Runt { other } if !validate_runt(&EdgeTrigger { level: value, ..settings.trigger() }, other)) { return Err(value_range_error(item, unit)); }
            settings.set_trigger_level(value)
        }
        Item::Control(Part::RuntLevel) if validate_runt(&settings.trigger(), value) => settings.set_runt(Some(value)),
        Item::Holdoff if value >= 0.0 => settings.set_holdoff(value),
        Item::Control(Part::PositionMarker) if (0.0..=100.0).contains(&value) => settings.set_position(value / 100.0),
        Item::Control(Part::CellDivider) if (20.0..=60.0).contains(&value) => settings.set_cell_share(value / 100.0),
        Item::Control(Part::Divider) if (15.0..=75.0).contains(&value) => settings.set_waterfall_share(value / 100.0),
        Item::Control(Part::SpectrumScale) if (0.1..=50.0).contains(&value) => settings.set_fft_view(settings.fft.ref_dbv, value),
        Item::Control(Part::SpectrumOffset) => settings.set_fft_view(value, settings.fft.db_per_div),
        Item::Control(Part::Time | Part::Frequency) if (DIV_MIN..=DIV_MAX).contains(&value) => {
            let span = value * 10.0;
            let window = Window { pre: span * settings.position, post: span * (1.0 - settings.position) };
            let changed = settings.time_per_div != value;
            let position = settings.position;
            settings.set_view(Some(window), None);
            // Preserve the parsed division and position after the window's floating-point round trip.
            settings.time_per_div = value;
            settings.position = position;
            if changed { settings.actions(false) } else { Vec::new() }
        }
        Item::Control(Part::Scale(i)) if (DIV_MIN..=DIV_MAX).contains(&value) => {
            let changed = settings.set_view(None, Some((i, value, settings.channels[i].offset_div)));
            if settings.channels[i].volts_per_div != value { return Err("out of range: scale must give a finite, nonzero value span".into()); }
            if changed { settings.actions(false) } else { Vec::new() }
        }
        Item::Control(Part::Offset(i)) if (value / settings.channels[i].volts_per_div).is_finite() => settings.set_offset(i, value / settings.channels[i].volts_per_div),
        _ => return Err(value_range_error(item, unit)),
    };
    Ok(actions)
}

fn set_value(settings: &mut ScopeSettings, item: Item, request: &a11y_tree::ActionRequest) -> Result<Vec<ScopeAction>, ActionResult> {
    if request.action != A::SET_VALUE { return Err(ActionResult::Unsupported); }
    match &request.arg {
        ActionArg::Text(text) => set_value_text(settings, item, text),
        ActionArg::Number(value) => {
            let unit = value_unit(settings, item).ok_or(ActionResult::Unsupported)?;
            apply_value(settings, item, *value, &unit)
        }
        _ => return Err(ActionResult::Unsupported),
    }.map_err(|_| ActionResult::Unsupported)
}

fn height_value(request: &a11y_tree::ActionRequest) -> Result<ResizeOutcome, ActionResult> {
    if request.action != A::SET_VALUE { return Err(ActionResult::Unsupported); }
    let value = semantics::number(&request.arg, "pt").filter(|value| *value >= MIN_LANE_HEIGHT).ok_or(ActionResult::Unsupported)?;
    Ok(resize(value, 0.0, false))
}

fn bracket_value(item: Item, old: (f64, f64), arg: &ActionArg, unit: &str) -> Option<(f64, f64)> {
    let pair = match item {
        Item::TimeBracket | Item::ValueBracket => semantics::pair(arg, unit),
        Item::TimeEnd(end) | Item::ValueEnd(end) => semantics::number(arg, unit).map(|v| if end == End::Start { (v, old.1) } else { (old.0, v) }),
        _ => None,
    }?;
    (pair.0 <= pair.1).then_some(pair)
}

impl SignalLane {
    fn value_entry_rect(&self, cx: &Cx, part: Part) -> Option<Rect> {
        if part == Part::Holdoff { return makepad_platform::a11y::snapshot_rect(&self.holdoff_area, cx); }
        let rect = self.parts.iter().rev().find(|p| p.part == part)?.rect;
        let shift = if self.shared_time && part == Part::Time { dvec2(0.0, 0.0) } else { self.area.rect(cx).pos - self.drawn_at };
        Some(Rect { pos: rect.pos + shift, ..rect })
    }

    fn value_popover_widget(&mut self, cx: &mut Cx) -> WidgetRef {
        if self.value_popover_widget.is_none() {
            self.value_popover_widget = Some(cx.with_vm(|vm| WidgetRef::script_from_value(vm, self.value_popover.as_object().into())));
            cx.widget_tree_mark_dirty(self.uid);
        }
        self.value_popover_widget.as_ref().unwrap().clone()
    }

    pub(super) fn open_value_entry(&mut self, cx: &mut Cx, part: Part) -> bool {
        let Some(text) = value_text(&self.settings, value_item(part)) else { return false };
        // A numeric control with no drawn anchor must not fall back to Enter's stepping action.
        let Some(rect) = self.value_entry_rect(cx, part) else { return true };
        let popover = self.value_popover_widget(cx).as_popover();
        let content = popover.content();
        let input = content.text_input(cx, ids!(input));
        input.set_text(cx, &text);
        if let Some(mut field) = input.borrow_mut() { field.select_all(cx); }
        content.label(cx, ids!(error)).set_visible(cx, false);
        self.focused = Some(part);
        self.drag = None;
        self.runt_preview = None;
        self.view_drag = None;
        self.holdoff_grab = None;
        self.pointer = None;
        // The popover focuses its first input after drawing it and restores this area on close.
        cx.set_key_focus_from_user(self.area);
        popover.open_at(cx, rect);
        self.value_entry_part = Some(part);
        self.value_anchor = Some(rect);
        self.area.redraw(cx);
        true
    }

    fn value_entry_actions(&mut self, cx: &mut Cx, popover: &PopoverRef, actions: &Actions) {
        let Some(part) = self.value_entry_part.filter(|_| popover.is_open()) else { return };
        let content = popover.content();
        let input = content.text_input(cx, ids!(input));
        if input.escaped(actions) { popover.close(cx); return; }
        if let Some((text, _)) = input.returned(actions) {
            match set_value_text(&mut self.settings, value_item(part), &text) {
                Ok(actions) => {
                    self.update_stretch();
                    self.emit(cx, actions);
                    popover.close(cx);
                }
                Err(reason) => {
                    let error = content.label(cx, ids!(error));
                    let ink = InstrumentLook::for_theme(self.dark).canvas.violation;
                    error.set_text_color(cx, vec4(ink[0], ink[1], ink[2], ink[3]));
                    error.set_text(cx, &reason);
                    error.set_visible(cx, true);
                    input.set_key_focus(cx);
                    content.redraw(cx);
                }
            }
        }
    }

    pub(super) fn handle_value_entry(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) -> bool {
        let Some(widget) = self.value_popover_widget.clone() else { return false };
        let popover = widget.as_popover();
        let was_open = popover.is_open();
        if was_open {
            let anchor = self.value_entry_part.and_then(|part| self.value_entry_rect(cx, part));
            if anchor != self.value_anchor {
                if let Some(rect) = anchor { popover.open_at(cx, rect); }
                else { popover.close(cx); }
                self.value_anchor = anchor;
            }
        }
        let actions = cx.capture_actions(|cx| widget.handle_event(cx, event, scope));
        if was_open {
            self.value_entry_actions(cx, &popover, &actions);
            if let Event::Actions(actions) = event { self.value_entry_actions(cx, &popover, actions); }
            if !popover.is_open() {
                self.value_entry_part = None;
                self.value_anchor = None;
                self.pointer = None;
                self.area.redraw(cx);
            }
        }
        // All input, including Escape and the closing outside press, belongs to the entry.
        was_open
    }

    pub(super) fn draw_value_entry(&mut self, cx: &mut Cx2d, scope: &mut Scope) {
        let widget = self.value_popover_widget(cx);
        // Draw closed too: give the focus/pointer trap an area before the first open and clear old
        // overlays. The absolute walk adds no lane space, even when an error line is visible.
        // A shared time anchor is drawn later by the bench, so keep the event-side rect here.
        let anchor = self.value_anchor.unwrap_or_else(|| self.area.rect(cx));
        widget.draw_walk_all(cx, scope, Walk::abs_rect(anchor));
    }

    fn answer_rows_origin(&self) -> DVec2 {
        dvec2(self.drawn_at.x + PAD, self.plot.pos.y + self.plot.size.y + if self.shared_time { 0.0 } else { 22.0 } + 1.0)
    }

    pub(super) fn answer_part_rect(&self, part: Part) -> Option<Rect> {
        self.parts.iter().find(|p| p.part == part).map(|p| p.rect).or_else(|| match part {
            Part::Answer(i) => wrapped_rect(&self.answer_widths, &self.answer_indices, &self.answers_layout.rows, self.answer_rows_origin(), i), _ => None,
        })
    }

    pub(super) fn wrapped_answer_at(&self, point: DVec2) -> Option<Part> {
        self.answers_layout.rows.iter().flat_map(|range| range.clone()).filter_map(|i| self.answer_indices.get(i).copied()).find(|&i| self.answer_part_rect(Part::Answer(i)).is_some_and(|rect| rect.contains(point))).map(Part::Answer)
    }

    pub(in crate::instruments) fn attach_summary(&mut self, summary: Rc<std::cell::RefCell<semantics::Summary>>) {
        summary.borrow_mut().answer(&self.net, &self.answers.text);
        self.semantic_summary = Some(summary);
    }

    fn reveal_answer(&mut self, cx: &mut Cx, part: Part) {
        let anchor = if self.answer_part_rect(part).is_some() { part }
            else if self.parts.iter().any(|p| p.part == Part::More) { Part::More } else { Part::Tag };
        if self.answers_popover_widget.as_ref().is_none_or(|widget| !widget.as_popover().is_open()) { self.toggle_answers(cx, anchor); }
    }

    fn semantic_items(&self, cx: &Cx) -> Items<'_> {
        Items {
            net: &self.net, settings: &self.settings, answers: &self.answers, names: &self.answer_names, parts: &self.parts,
            widths: &self.answer_widths, indices: &self.answer_indices, rows: &self.answers_layout.rows, rows_origin: self.answer_rows_origin(),
            sources: &self.source_labels, primary: self.primary_source, more: self.answers_layout.more,
            trigger: self.trigger_lane, shared: self.shared_time, collapsed: self.collapsed, closable: self.closable,
            bracket_visible: self.bracket_visible, bracket: self.bracket, values: self.value_bracket, delays: &self.delay_marks,
            plot: self.plot, shift: self.area.rect(cx).pos - self.drawn_at,
            clip: makepad_platform::a11y::snapshot_rect(&self.area, cx), time: makepad_platform::a11y::snapshot_rect(&self.time_area, cx),
            holdoff: makepad_platform::a11y::snapshot_rect(&self.holdoff_area, cx), focused: self.focused.filter(|_| cx.has_key_focus(self.area)),
            lane: Some(self),
        }
    }

    pub(in crate::instruments) fn delay_lookup_rect(&self, cx: &Cx, index: usize, end: End) -> Option<Rect> {
        let mark = self.delay_marks.get(index)?;
        if mark.end != Some(end) { return None; }
        let edge = mark.feature?;
        let plot = self.delay_plot(cx)?;
        let shift = self.area.rect(cx).pos - self.drawn_at;
        let x = self.bracket_x(edge.t) + shift.x;
        let clip = makepad_platform::a11y::snapshot_rect(&self.area, cx)?;
        let shown = semantics::intersect(plot, clip)?;
        if x < shown.pos.x || x >= shown.pos.x + shown.size.x { return None; }
        let point = dvec2(x, shown.pos.y + shown.size.y * 0.5);
        let rect = semantics::intersect(Rect { pos: point - dvec2(1.0, 1.0), size: dvec2(2.0, 2.0) }, shown)?;
        let (x, y) = makepad_platform::a11y::semantic_rect(rect).center();
        self.delay_end(dvec2(x as f64, y as f64) - shift).filter(|(i, e, _)| *i == index && *e == end).map(|_| rect)
    }

    fn reveal_time(&mut self, cx: &mut Cx, a: f64, b: f64) {
        let window = self.settings.window();
        if a >= -window.pre && b <= window.post { return; }
        let margin = self.settings.time_per_div;
        self.apply_view(cx, Some(Window { pre: window.pre.max(-a + margin), post: window.post.max(b + margin) }), None);
    }

    fn reveal_value(&mut self, cx: &mut Cx, a: f64, b: f64) {
        let Some(channel) = self.trace_channel() else { return };
        let (lo, hi) = self.value_span(channel);
        if a >= lo && b <= hi { return; }
        let scale = self.settings.channels[channel].volts_per_div.max((b - a) / 6.0);
        self.apply_view(cx, None, Some((channel, scale, (a * 0.5 + b * 0.5) / scale)));
    }

    fn reveal_item(&mut self, cx: &mut Cx, item: Item) {
        if !matches!(item, Item::Control(Part::Tag | Part::Collapse | Part::Resize | Part::Close | Part::Answer(_) | Part::More | Part::AddAnswer)) { self.set_collapsed(cx, false); }
        match item {
            Item::TimeBracket => { if let Some((a, b)) = self.bracket { self.reveal_time(cx, a, b); } }
            Item::TimeEnd(end) => { if let Some((a, b)) = self.bracket { let t = if end == End::Start { a } else { b }; self.reveal_time(cx, t, t); } }
            Item::ValueBracket => { if let Some((a, b)) = self.value_bracket { self.reveal_value(cx, a, b); } }
            Item::ValueEnd(end) => { if let Some((a, b)) = self.value_bracket { let v = if end == End::Start { a } else { b }; self.reveal_value(cx, v, v); } }
            Item::Control(Part::Zone) => {
                if let Some(zone) = self.lane_zone() { self.reveal_time(cx, zone.t0, zone.t1); self.reveal_value(cx, zone.v0, zone.v1); }
            }
            Item::Control(Part::LevelMarker) => self.reveal_value(cx, self.settings.trigger.level, self.settings.trigger.level),
            Item::Control(Part::RuntLevel) => {
                if let Qualifier::Runt { other } = self.settings.trigger.qualify { self.reveal_value(cx, other, other); }
            }
            Item::Control(part @ Part::Answer(_)) => self.reveal_answer(cx, part),
            _ => {}
        }
    }

    pub(in crate::instruments) fn focus_delay_lookup(&mut self, cx: &mut Cx, index: usize, end: End, reveal: bool) {
        if reveal {
            self.set_collapsed(cx, false);
            if let Some(t) = self.delay_marks.get(index).and_then(|mark| mark.feature).map(|edge| edge.t) { self.reveal_time(cx, t, t); }
        }
        self.focused = self.trace_channel().map(Part::Plot);
        if let Some(rect) = self.delay_lookup_rect(cx, index, end) { self.pointer = Some(rect.pos + rect.size * 0.5); }
        cx.set_key_focus(self.area);
        self.area.redraw(cx);
    }

    pub(in crate::instruments) fn lookup(&self, cx: &Cx, lookup: &Lookup, out: &mut Vec<(u64, Option<u64>, Node)>) { self.semantic_items(cx).lookup(lookup, out); }

    pub(in crate::instruments) fn lookup_action(&mut self, cx: &mut Cx, key: u64, request: &a11y_tree::ActionRequest) -> ActionResult {
        let Some(item) = self.semantic_items(cx).find(key) else { return ActionResult::Unhandled };
        if request.action == A::DISMISS {
            let Item::Control(Part::Answer(i)) = item else { return ActionResult::Unsupported };
            let Some(name) = self.answer_names.get(i).copied().flatten() else { return ActionResult::Unsupported };
            if !self.remove_answer(cx, name) { return ActionResult::Unsupported; }
        } else if request.action == A::SET_VALUE {
            if item == Item::Control(Part::Resize) {
                let outcome = match height_value(request) { Ok(outcome) => outcome, Err(error) => return error };
                self.apply_resize(cx, outcome, true);
                return ActionResult::Handled;
            }
            let old = match item { Item::TimeBracket | Item::TimeEnd(_) => self.bracket, Item::ValueBracket | Item::ValueEnd(_) => self.value_bracket, _ => None };
            if let Some((a, b)) = old {
                let unit = if matches!(item, Item::TimeBracket | Item::TimeEnd(_)) { "s" } else { self.settings.channels.first().map_or("V", |c| c.unit.as_str()) };
                let value = bracket_value(item, (a, b), &request.arg, unit);
                let Some(value) = value else { return ActionResult::Unsupported };
                if matches!(item, Item::TimeBracket | Item::TimeEnd(_)) { self.edit_bracket(cx, Some(value)); }
                else { self.edit_value_bracket(cx, Some(value)); }
            } else {
                let actions = match set_value(&mut self.settings, item, request) { Ok(actions) => actions, Err(error) => return error };
                self.update_stretch();
                self.emit(cx, actions);
            }
        } else if request.action == A::FOCUS || request.action == A::SCROLL_INTO_VIEW || request.action == A::PRESS {
            let part = match item { Item::Control(part) => part, Item::Holdoff => Part::Holdoff, _ => Part::Plot(self.trace_channel().unwrap_or(0)) };
            if request.action == A::SCROLL_INTO_VIEW { self.reveal_item(cx, item); }
            self.focused = Some(part);
            cx.set_key_focus(self.area);
            if request.action == A::PRESS {
                if matches!(part, Part::Answer(_)) {
                    if self.answer_part_rect(part).is_some() { self.toggle_answers(cx, part); } else { self.reveal_answer(cx, part); }
                } else if part.glyph() { self.adjust(cx, part, 1); }
            }
            self.area.redraw(cx);
        } else { return ActionResult::Unsupported; }
        ActionResult::Handled
    }
}

#[cfg(test)]
#[path = "scope_nodes_tests.rs"]
mod tests;
